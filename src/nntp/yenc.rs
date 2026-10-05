//! yEnc decoder with full header parsing.
//!
//! yEnc segments include `=ybegin`, `=ypart`, and `=yend` headers that carry the
//! authoritative byte offset, size, and CRC of the decoded payload. We rely on
//! the `=ypart begin=X end=Y` header to place each segment in the output file,
//! because the segment's encoded size (carried in the NZB) is not the decoded
//! size — using the encoded size as an offset leaves zero-filled gaps inside
//! every multi-segment file.
//!
//! Per yEnc-1.3 / yenc-1.0 specs:
//! - `=ybegin` may carry `line=`, `size=`, `name=`, and (for multi-part) `part=`, `total=`.
//! - `=ypart` carries `begin=` and `end=`, both **1-indexed and end-inclusive**.
//! - `=yend` carries `size=`, optionally `pcrc32=` (per-part) and `crc32=` (whole file).

use crate::error::{DlNzbError, NntpError};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Result of decoding a single yEnc-encoded article body.
#[derive(Debug)]
pub(crate) struct YencDecoded {
    pub data: Vec<u8>,
    /// Zero-indexed byte offset into the final assembled file.
    pub offset: u64,
    /// True iff a pcrc32/crc32 checksum was present and matched (reaching Ok with a present checksum implies it matched).
    pub crc_verified: bool,
}

/// `=ypart` header: 1-indexed `begin`, inclusive `end`.
struct YPart {
    begin: u64,
    end: u64,
}

/// The most room made for decoded bytes ahead of the data, on the article's
/// own word (`=ypart`, `=ybegin size=`). A part is rarely over 1 MiB; a
/// larger one grows as it decodes, and one that only claims to be huge
/// can't make it allocate more than this.
const RESERVE_MAX: u64 = 4 * 1024 * 1024;

/// A yEnc article body decoded line by line as it arrives (the connection
/// hands over each line straight from its read buffer), then checked by
/// [`finish`](Self::finish). Lines before `=ybegin` are skipped, and so are
/// those after `=yend`.
#[derive(Default)]
pub(crate) struct Decoder {
    stage: Stage,
    ybegin_size: Option<u64>,
    ypart: Option<YPart>,
    decoded: Vec<u8>,
    yend_size: Option<u64>,
    yend_pcrc32: Option<u32>,
}

#[derive(Default)]
enum Stage {
    /// Looking for `=ybegin`.
    #[default]
    Header,
    /// Decoding the data, up to `=yend`.
    Data,
    /// `=yend` seen.
    Ended,
    /// The body is bad (an invalid `=ypart`); the rest of it is ignored.
    Failed(String),
}

impl Decoder {
    /// Take the body's next line, without its line end (a CR left at the
    /// end is dropped too).
    pub(crate) fn line(&mut self, line: &[u8]) {
        let line = strip_cr(line);
        match self.stage {
            Stage::Header => {
                if line.starts_with(b"=ybegin") {
                    self.ybegin_size = parse_decimal_attr(line, b"size=");
                    self.stage = Stage::Data;
                }
            }
            Stage::Data => self.data_line(line),
            Stage::Ended | Stage::Failed(_) => {}
        }
    }

    fn data_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        if line.starts_with(b"=ypart") {
            match ypart(line, self.ybegin_size) {
                Ok(part) => self.ypart = Some(part),
                Err(message) => self.stage = Stage::Failed(message),
            }
            return;
        }
        if line.starts_with(b"=yend") {
            self.yend_size = parse_decimal_attr(line, b"size=");
            // Multi-part may also carry pcrc32 only; single-part uses crc32=.
            // We treat both names as the per-part checksum since for single-part
            // it covers the whole file.
            self.yend_pcrc32 =
                parse_hex_attr(line, b"pcrc32=").or_else(|| parse_hex_attr(line, b"crc32="));
            self.stage = Stage::Ended;
            return;
        }
        // Unknown `=y...` header (e.g. =yparta, future extensions): skip.
        if line.starts_with(b"=y") {
            return;
        }
        if self.decoded.capacity() == 0 {
            // Room for the whole part at once (up to `RESERVE_MAX`).
            let expected = match &self.ypart {
                Some(p) => p.end - p.begin + 1,
                None => self.ybegin_size.unwrap_or(0),
            };
            self.decoded.reserve(expected.min(RESERVE_MAX) as usize);
        }
        decode_line(line, &mut self.decoded);
    }

    /// The decoded part and its placement in the file, once the whole body
    /// has been taken. The decoded size must match `=ypart`'s
    /// `end - begin + 1` (or `=yend size=`, then `=ybegin size=`, for
    /// single-part) and, when present, the `pcrc32` checksum.
    pub(crate) fn finish(self) -> Result<YencDecoded> {
        let fail =
            |message: String| -> Result<YencDecoded> { Err(NntpError::YencDecode(message).into()) };
        match self.stage {
            Stage::Header => return fail("missing =ybegin header".to_string()),
            Stage::Data => return fail("missing =yend trailer".to_string()),
            Stage::Failed(message) => return fail(message),
            Stage::Ended => {}
        }
        let decoded = self.decoded;

        let expected_size: u64 = match (self.ypart.as_ref(), self.yend_size, self.ybegin_size) {
            (Some(p), _, _) => p.end - p.begin + 1,
            (None, Some(s), _) => s,
            (None, None, Some(s)) => s,
            (None, None, None) => decoded.len() as u64,
        };
        if (decoded.len() as u64) != expected_size {
            return fail(format!(
                "size mismatch: decoded {} bytes, expected {}",
                decoded.len(),
                expected_size
            ));
        }

        if let Some(expected_crc) = self.yend_pcrc32 {
            let actual = crc32_ieee(&decoded);
            if actual != expected_crc {
                return fail(format!(
                    "CRC32 mismatch: got {:08x} expected {:08x}",
                    actual, expected_crc
                ));
            }
        }

        let offset = match self.ypart {
            Some(p) => p.begin - 1, // convert to 0-indexed
            None => 0,
        };

        Ok(YencDecoded {
            data: decoded,
            offset,
            crc_verified: self.yend_pcrc32.is_some(),
        })
    }
}

/// An `=ypart` line's range, which must lie inside the file `=ybegin size=`
/// describes. (The downloader also holds it to a limit the NZB sets: the
/// article sets both numbers, so this alone can't stop a part placed far
/// past the end.)
fn ypart(line: &[u8], file_size: Option<u64>) -> std::result::Result<YPart, String> {
    let begin = parse_decimal_attr(line, b"begin=").ok_or("=ypart missing begin=")?;
    let end = parse_decimal_attr(line, b"end=").ok_or("=ypart missing end=")?;
    if begin == 0 || end < begin || file_size.is_some_and(|size| end > size) {
        return Err(format!(
            "=ypart invalid range begin={} end={} (size={:?})",
            begin, end, file_size
        ));
    }
    Ok(YPart { begin, end })
}

/// Decode a whole article body at once, its lines ending in LF or CRLF.
#[cfg(test)]
pub(crate) fn decode_article(body: &[u8]) -> Result<YencDecoded> {
    let mut decoder = Decoder::default();
    let mut rest = body;
    while let Some(end) = memchr::memchr(b'\n', rest) {
        decoder.line(&rest[..end]);
        rest = &rest[end + 1..];
    }
    decoder.line(rest);
    decoder.finish()
}

#[inline]
fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

/// Decode one yEnc payload line: `=` escapes the byte after it (one at the
/// very end escapes nothing), a stray CR is dropped, and every other byte is
/// shifted down by 42. The runs between escapes, nearly all of a line, are
/// found with `memchr` and shifted in bulk.
fn decode_line(line: &[u8], output: &mut Vec<u8>) {
    // A line decodes to at most its own length, so room for that is made
    // once and the bytes are written straight into it.
    output.reserve(line.len());
    let start = output.len();
    let out = &mut output.spare_capacity_mut()[..line.len()];
    let mut written = 0;
    let mut rest = line;
    loop {
        let run = memchr::memchr2(b'=', b'\r', rest).unwrap_or(rest.len());
        shift_run(&rest[..run], &mut out[written..written + run]);
        written += run;
        rest = match (rest.get(run), rest.get(run + 1)) {
            (None, _) => break,
            (Some(b'='), Some(&escaped)) => {
                out[written].write(escaped.wrapping_sub(64 + 42));
                written += 1;
                &rest[run + 2..]
            }
            _ => &rest[run + 1..],
        };
    }
    // SAFETY: the `written` bytes after `start` were all just written, and
    // `reserve` made room for them.
    unsafe { output.set_len(start + written) };
}

/// Shift each byte of `run` down by 42 into `out` (of the same length).
fn shift_run(run: &[u8], out: &mut [std::mem::MaybeUninit<u8>]) {
    for (o, &b) in out.iter_mut().zip(run) {
        o.write(b.wrapping_sub(42));
    }
}

/// Parse `key=NNN` from a header line, where NNN is a positive decimal integer.
fn parse_decimal_attr(line: &[u8], key: &[u8]) -> Option<u64> {
    let pos = find_subseq(line, key)?;
    let rest = &line[pos + key.len()..];
    let end = rest
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
}

/// Parse `key=HEX` from a header line, where HEX is up to 8 hex digits.
fn parse_hex_attr(line: &[u8], key: &[u8]) -> Option<u32> {
    let pos = find_subseq(line, key)?;
    let rest = &line[pos + key.len()..];
    let end = rest
        .iter()
        .position(|&b| !b.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    u32::from_str_radix(std::str::from_utf8(&rest[..end]).ok()?, 16).ok()
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// --- CRC32 (IEEE 802.3, polynomial 0xEDB88320) ---

fn crc32_ieee(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Articles as posters make them, for tests and timings.
#[cfg(test)]
pub(crate) mod sample {
    /// `len` bytes of noise (the same for the same `seed`), like a
    /// compressed file's.
    pub(crate) fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Part 1 of a file that is `plain`, as a yEnc body: lines of 128
    /// characters (an escape can make one 129) ending in CRLF.
    pub(crate) fn article(plain: &[u8]) -> Vec<u8> {
        let mut body = format!(
            "=ybegin part=1 total=1 line=128 size={0} name=sample.bin\r\n=ypart begin=1 end={0}\r\n",
            plain.len()
        )
        .into_bytes();
        let mut column = 0;
        for &b in plain {
            let enc = b.wrapping_add(42);
            if matches!(enc, b'\0' | b'\r' | b'\n' | b'=') {
                body.extend_from_slice(&[b'=', enc.wrapping_add(64)]);
                column += 2;
            } else {
                body.push(enc);
                column += 1;
            }
            if column >= 128 {
                body.extend_from_slice(b"\r\n");
                column = 0;
            }
        }
        if column > 0 {
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(
            format!(
                "=yend size={} part=1 pcrc32={:08x}\r\n",
                plain.len(),
                crc32fast::hash(plain)
            )
            .as_bytes(),
        );
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_line(plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + plain.len() / 16);
        for &b in plain {
            let enc = b.wrapping_add(42);
            // Per spec, escape NUL, LF, CR, '='
            if matches!(enc, b'\0' | b'\r' | b'\n' | b'=') {
                out.push(b'=');
                out.push(enc.wrapping_add(64));
            } else {
                out.push(enc);
            }
        }
        out
    }

    fn build_multipart(
        part: u32,
        begin: u64,
        end: u64,
        decoded: &[u8],
        include_crc: bool,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "=ybegin part={} line=128 size={} name=test.bin\r\n",
                part,
                end + 1
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("=ypart begin={} end={}\r\n", begin, end).as_bytes());
        body.extend_from_slice(&encode_line(decoded));
        body.extend_from_slice(b"\r\n");
        if include_crc {
            let crc = crc32_ieee(decoded);
            body.extend_from_slice(
                format!(
                    "=yend size={} part={} pcrc32={:08x}\r\n",
                    decoded.len(),
                    part,
                    crc
                )
                .as_bytes(),
            );
        } else {
            body.extend_from_slice(
                format!("=yend size={} part={}\r\n", decoded.len(), part).as_bytes(),
            );
        }
        body
    }

    #[test]
    fn decodes_simple_singlepart() {
        let plain = b"Hello, world!";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!("=ybegin line=128 size={} name=test.txt\r\n", plain.len()).as_bytes(),
        );
        body.extend_from_slice(&encode_line(plain));
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("=yend size={}\r\n", plain.len()).as_bytes());
        let decoded = decode_article(&body).unwrap();
        assert_eq!(decoded.data, plain);
        assert_eq!(decoded.offset, 0);
    }

    #[test]
    fn decodes_multipart_offsets_correctly() {
        // Two parts: bytes 1..=10 then 11..=20
        let part1_data: Vec<u8> = (0u8..10).collect();
        let part2_data: Vec<u8> = (10u8..20).collect();
        let body1 = build_multipart(1, 1, 10, &part1_data, true);
        let body2 = build_multipart(2, 11, 20, &part2_data, true);

        let d1 = decode_article(&body1).unwrap();
        let d2 = decode_article(&body2).unwrap();
        assert_eq!(d1.offset, 0);
        assert_eq!(d2.offset, 10);
        assert_eq!(d1.data, part1_data);
        assert_eq!(d2.data, part2_data);
    }

    #[test]
    fn rejects_crc_mismatch() {
        let plain = b"some data";
        let mut body = build_multipart(1, 1, plain.len() as u64, plain, true);
        // Corrupt the encoded byte
        let pos = body
            .iter()
            .position(|&b| b == plain[0].wrapping_add(42))
            .unwrap();
        body[pos] = body[pos].wrapping_add(1);
        let err = decode_article(&body).unwrap_err();
        assert!(err.to_string().contains("CRC32"));
    }

    #[test]
    fn rejects_size_mismatch() {
        // Claim 20 bytes but supply 10
        let data: Vec<u8> = (0u8..10).collect();
        let body = build_multipart(1, 1, 20, &data, false);
        let err = decode_article(&body).unwrap_err();
        assert!(err.to_string().contains("size mismatch"));
    }

    #[test]
    fn rejects_a_part_ending_past_the_file_size() {
        let data: Vec<u8> = (0u8..10).collect();
        // `=ybegin size=` says the file is 20 bytes; the part claims 91..=100.
        let mut body = b"=ybegin part=2 line=128 size=20 name=test.bin\r\n".to_vec();
        body.extend_from_slice(b"=ypart begin=91 end=100\r\n");
        body.extend_from_slice(&encode_line(&data));
        body.extend_from_slice(b"\r\n");
        let crc = crc32_ieee(&data);
        body.extend_from_slice(format!("=yend size=10 part=2 pcrc32={crc:08x}\r\n").as_bytes());
        let err = decode_article(&body).unwrap_err();
        assert!(err.to_string().contains("invalid range"), "{err}");
    }

    #[test]
    fn rejects_missing_ybegin() {
        let body = b"=ypart begin=1 end=10\r\n";
        assert!(decode_article(body).is_err());
    }

    #[test]
    fn rejects_missing_yend() {
        let body = b"=ybegin size=10\r\nabcdefghij\r\n";
        assert!(decode_article(body).is_err());
    }

    #[test]
    fn crc32_known_vectors() {
        // "123456789" CRC32-IEEE = 0xCBF43926
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        // Empty
        assert_eq!(crc32_ieee(b""), 0);
    }

    #[test]
    fn parse_decimal_attr_works() {
        let line = b"=ypart begin=12345 end=67890";
        assert_eq!(parse_decimal_attr(line, b"begin="), Some(12345));
        assert_eq!(parse_decimal_attr(line, b"end="), Some(67890));
        assert_eq!(parse_decimal_attr(line, b"missing="), None);
    }

    #[test]
    fn parse_hex_attr_works() {
        let line = b"=yend size=10 pcrc32=cbf43926";
        assert_eq!(parse_hex_attr(line, b"pcrc32="), Some(0xCBF4_3926));
    }

    /// The decoder before runs were shifted in bulk, a byte at a time.
    fn decode_line_bytewise(line: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut iter = line.iter().copied();
        while let Some(b) = iter.next() {
            if b == b'=' {
                if let Some(next) = iter.next() {
                    output.push(next.wrapping_sub(64).wrapping_sub(42));
                }
            } else if b != b'\r' {
                output.push(b.wrapping_sub(42));
            }
        }
        output
    }

    #[test]
    fn decode_line_matches_the_bytewise_decoder() {
        let mut lines: Vec<Vec<u8>> = [
            &b""[..],
            b"=",
            b"==",
            b"===",
            b"=\r",
            b"\r",
            b"\r\r",
            b"a=",
            b"=a",
            b"a\rb",
            b"abc=",
            b"=}=J=M=@",
            b"0123456789abcdefghijklmnopqrstuvwxyz=}",
        ]
        .iter()
        .map(|l| l.to_vec())
        .collect();
        // Lines thick with escapes and stray CRs, of every length to 300.
        let noise = sample::noise(300 * 301, 3);
        let mut at = 0;
        for len in 0..300 {
            let line = noise[at..at + len]
                .iter()
                .map(|&b| match b % 7 {
                    0 => b'=',
                    1 if b % 3 == 0 => b'\r',
                    _ => b,
                })
                .collect();
            lines.push(line);
            at += len;
        }
        for line in lines {
            let mut decoded = b"kept".to_vec();
            decode_line(&line, &mut decoded);
            assert_eq!(&decoded[..4], b"kept");
            assert_eq!(decoded[4..], decode_line_bytewise(&line), "{line:?}");
        }
    }

    #[test]
    fn decodes_bytes_that_all_need_escapes() {
        // NUL, LF, CR and `=` once shifted by 42: every byte is escaped, so
        // lines end on escapes too.
        let plain: Vec<u8> = [0xD6u8, 0xE0, 0xE3, 0x13].repeat(500);
        let body = sample::article(&plain);
        assert_eq!(decode_article(&body).unwrap().data, plain);
    }

    #[test]
    fn decodes_a_typical_article() {
        let plain = sample::noise(100_000, 5);
        let body = sample::article(&plain);
        let decoded = decode_article(&body).unwrap();
        assert_eq!(decoded.data, plain);
        assert!(decoded.crc_verified);
    }

    #[test]
    fn a_dangling_escape_at_a_line_end_is_dropped() {
        let mut body = b"=ybegin line=128 size=2 name=a\r\n".to_vec();
        // `=` ends the line, so it escapes nothing; the next line goes on.
        body.extend_from_slice(&[b'a' + 42, b'=', b'\r', b'\n', b'b' + 42, b'\r', b'\n']);
        body.extend_from_slice(b"=yend size=2\r\n");
        assert_eq!(decode_article(&body).unwrap().data, b"ab");
    }

    /// Decoding speed on a typical 700 KB article. Run with
    /// `cargo test --profile release-fast --lib -- --ignored --nocapture yenc_decode_speed`.
    #[test]
    #[ignore]
    fn yenc_decode_speed() {
        let plain = sample::noise(700_000, 7);
        let body = sample::article(&plain);
        let rounds = 2_000;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let decoded = decode_article(std::hint::black_box(&body)).unwrap();
            assert_eq!(decoded.data.len(), plain.len());
        }
        let secs = started.elapsed().as_secs_f64();
        println!(
            "yEnc decode: {:.0} MB/s, {:.0} us per article",
            (body.len() * rounds) as f64 / secs / 1e6,
            secs / rounds as f64 * 1e6
        );
    }
}
