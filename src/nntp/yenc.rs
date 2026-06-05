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

/// Decode a single yEnc article body to bytes plus its placement in the file.
///
/// The decoded size must match `=ypart`'s `end - begin + 1` (or `=ybegin size=`
/// for single-part) and, when present, the `pcrc32` checksum.
pub(crate) fn decode_article(body: &[u8]) -> Result<YencDecoded> {
    let mut iter = body.split(|&b| b == b'\n');

    let ybegin_size = iter
        .by_ref()
        .map(strip_cr)
        .find(|l| l.starts_with(b"=ybegin"))
        .ok_or_else(|| NntpError::YencDecode("missing =ybegin header".to_string()))
        .map(|l| parse_decimal_attr(l, b"size="))?;

    // 2) Optional =ypart header (required for multi-part).
    // 3) Decoded payload — accumulated until we hit =yend.
    let mut ypart: Option<YPart> = None;
    let mut decoded: Vec<u8> = Vec::with_capacity(body.len()); // decoded <= encoded
    let mut found_yend = false;
    let mut yend_size: Option<u64> = None;
    let mut yend_pcrc32: Option<u32> = None;

    for line in iter {
        let line = strip_cr(line);
        if line.is_empty() {
            continue;
        }
        if line.starts_with(b"=ypart") {
            let begin = parse_decimal_attr(line, b"begin=")
                .ok_or_else(|| NntpError::YencDecode("=ypart missing begin=".to_string()))?;
            let end = parse_decimal_attr(line, b"end=")
                .ok_or_else(|| NntpError::YencDecode("=ypart missing end=".to_string()))?;
            if begin == 0 || end < begin {
                return Err(NntpError::YencDecode(format!(
                    "=ypart invalid range begin={} end={}",
                    begin, end
                ))
                .into());
            }
            ypart = Some(YPart { begin, end });
            continue;
        }
        if line.starts_with(b"=yend") {
            yend_size = parse_decimal_attr(line, b"size=");
            yend_pcrc32 = parse_hex_attr(line, b"pcrc32=");
            // Multi-part may also carry pcrc32 only; single-part uses crc32=.
            // We treat both names as the per-part checksum since for single-part
            // it covers the whole file.
            if yend_pcrc32.is_none() {
                yend_pcrc32 = parse_hex_attr(line, b"crc32=");
            }
            found_yend = true;
            break;
        }
        // Unknown `=y...` header (e.g. =yparta, future extensions): skip.
        if line.starts_with(b"=y") {
            continue;
        }
        decode_line(line, &mut decoded);
    }

    if !found_yend {
        return Err(NntpError::YencDecode("missing =yend trailer".to_string()).into());
    }

    // 4) Determine expected size and offset.
    let expected_size: u64 = match (ypart.as_ref(), yend_size, ybegin_size) {
        (Some(p), _, _) => p.end - p.begin + 1,
        (None, Some(s), _) => s,
        (None, None, Some(s)) => s,
        (None, None, None) => decoded.len() as u64,
    };

    if (decoded.len() as u64) != expected_size {
        return Err(NntpError::YencDecode(format!(
            "size mismatch: decoded {} bytes, expected {}",
            decoded.len(),
            expected_size
        ))
        .into());
    }

    // 5) Verify CRC32 if present.
    if let Some(expected_crc) = yend_pcrc32 {
        let actual = crc32_ieee(&decoded);
        if actual != expected_crc {
            return Err(NntpError::YencDecode(format!(
                "CRC32 mismatch: got {:08x} expected {:08x}",
                actual, expected_crc
            ))
            .into());
        }
    }

    let offset = match ypart {
        Some(p) => p.begin - 1, // convert to 0-indexed
        None => 0,
    };

    Ok(YencDecoded {
        data: decoded,
        offset,
        crc_verified: yend_pcrc32.is_some(),
    })
}

#[inline]
fn strip_cr(line: &[u8]) -> &[u8] {
    if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    }
}

/// Decode a single yEnc payload line, applying escape sequences. We process
/// scalar/SIMD-friendly cases inline (the line-by-line work is dominated by
/// scanning for escapes; SIMD-subtracting 42 is already cheap).
fn decode_line(line: &[u8], output: &mut Vec<u8>) {
    let has_special = line.iter().any(|&b| b == b'=' || b == b'\r');
    if has_special {
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
    } else {
        decode_simd(line, output);
    }
}

#[cfg(target_arch = "x86_64")]
// `reserve` + `set_len` leaves the region uninitialized, but the SIMD loop below
// writes all `simd_len` bytes before any read, so the lint's concern is moot.
#[allow(clippy::uninit_vec)]
fn decode_simd(line: &[u8], output: &mut Vec<u8>) {
    use std::arch::x86_64::*;
    let len = line.len();
    let start = output.len();
    let chunks = len / 16;
    let simd_len = chunks * 16;
    if chunks > 0 {
        output.reserve(simd_len);
        // SAFETY: the SIMD loop below writes exactly `simd_len` bytes into this
        // region before any read; reserve guarantees the capacity.
        unsafe {
            output.set_len(start + simd_len);
        }
        // SAFETY: SSE2 is mandatory on x86_64; output was just resized to hold
        // the SIMD writes; pointers stay in-bounds for `chunks * 16` bytes.
        unsafe {
            let sub_val = _mm_set1_epi8(42);
            for i in 0..chunks {
                let off = i * 16;
                let input = _mm_loadu_si128(line.as_ptr().add(off) as *const __m128i);
                let result = _mm_sub_epi8(input, sub_val);
                _mm_storeu_si128(output.as_mut_ptr().add(start + off) as *mut __m128i, result);
            }
        }
    }
    for &b in &line[simd_len..] {
        output.push(b.wrapping_sub(42));
    }
}

#[cfg(target_arch = "aarch64")]
// `reserve` + `set_len` leaves the region uninitialized, but the SIMD loop below
// writes all `simd_len` bytes before any read, so the lint's concern is moot.
#[allow(clippy::uninit_vec)]
fn decode_simd(line: &[u8], output: &mut Vec<u8>) {
    use std::arch::aarch64::*;
    let len = line.len();
    let start = output.len();
    let chunks = len / 16;
    let simd_len = chunks * 16;
    if chunks > 0 {
        output.reserve(simd_len);
        // SAFETY: the SIMD loop below writes exactly `simd_len` bytes into this
        // region before any read; reserve guarantees the capacity.
        unsafe {
            output.set_len(start + simd_len);
        }
        // SAFETY: NEON is mandatory on aarch64; output was resized to fit;
        // pointers remain in-bounds for `chunks * 16` bytes.
        unsafe {
            let sub_val = vdupq_n_u8(42);
            for i in 0..chunks {
                let off = i * 16;
                let input = vld1q_u8(line.as_ptr().add(off));
                let result = vsubq_u8(input, sub_val);
                vst1q_u8(output.as_mut_ptr().add(start + off), result);
            }
        }
    }
    for &b in &line[simd_len..] {
        output.push(b.wrapping_sub(42));
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn decode_simd(line: &[u8], output: &mut Vec<u8>) {
    for &b in line {
        output.push(b.wrapping_sub(42));
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
}
