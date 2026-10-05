//! Builds tiny RAR5 archives for tests: stored (uncompressed) members, links
//! and file copies, optionally solid or one volume of a set. Just enough of the format
//! (<https://www.rarlab.com/technote.htm>) for unRAR to list and unpack them,
//! since no `rar` binary is available to make real ones.
//!
//! Shared by the unit tests in `src/processing` and the integration tests
//! (included with `#[path]`).
#![allow(dead_code)]

/// Redirection types (the file system redirection record).
pub const UNIX_SYMLINK: u64 = 1;
pub const HARDLINK: u64 = 4;
pub const FILE_COPY: u64 = 5;

enum Member {
    File(Vec<u8>),
    Link {
        kind: u64,
        target: String,
    },
    /// Part of a file split across volumes; `crc` is the whole file's.
    Part {
        data: Vec<u8>,
        before: bool,
        after: bool,
        crc: u32,
    },
}

/// A RAR5 archive being described.
#[derive(Default)]
pub struct Rar5 {
    solid: bool,
    /// Volume number (0 for the first) and whether another follows.
    volume: Option<(u64, bool)>,
    members: Vec<(String, Member)>,
}

impl Rar5 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the archive solid (the main header's flag): unRAR then has to
    /// read through every member it skips.
    pub fn solid(mut self) -> Self {
        self.solid = true;
        self
    }

    /// Volume `number` (0 for the first) of a set, followed by another one
    /// when `more` (named `<set>.partN.rar`, N counting from 1).
    pub fn volume(mut self, number: u64, more: bool) -> Self {
        self.volume = Some((number, more));
        self
    }

    /// The part of a stored file split across volumes held in this one:
    /// `before`/`after` say whether it continues from the previous volume or
    /// into the next; `whole` is the complete file (for its checksum).
    pub fn part(
        mut self,
        name: &str,
        data: &[u8],
        before: bool,
        after: bool,
        whole: &[u8],
    ) -> Self {
        self.members.push((
            name.to_string(),
            Member::Part {
                data: data.to_vec(),
                before,
                after,
                crc: crc32fast::hash(whole),
            },
        ));
        self
    }

    /// A stored member.
    pub fn file(mut self, name: &str, data: &[u8]) -> Self {
        self.members
            .push((name.to_string(), Member::File(data.to_vec())));
        self
    }

    /// A member that is a link (or file copy) of `kind` to `target`.
    pub fn link(mut self, name: &str, kind: u64, target: &str) -> Self {
        self.members.push((
            name.to_string(),
            Member::Link {
                kind,
                target: target.to_string(),
            },
        ));
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut out = b"Rar!\x1a\x07\x01\x00".to_vec();
        // Main header: type 1, no header flags, archive flags (volume = 1,
        // volume number present = 2, solid = 4).
        let mut main = Vec::new();
        vint(1, &mut main);
        vint(0, &mut main);
        let mut flags = if self.solid { 0x04 } else { 0 };
        match self.volume {
            Some((0, _)) => flags |= 0x01,
            Some(_) => flags |= 0x03,
            None => {}
        }
        vint(flags, &mut main);
        if let Some((number, _)) = self.volume.filter(|(n, _)| *n > 0) {
            vint(number, &mut main);
        }
        block(&mut out, &main, &[]);

        for (name, member) in &self.members {
            let mut split = 0;
            let (data, extra, crc) = match member {
                Member::File(data) => (data.clone(), Vec::new(), crc32fast::hash(data)),
                Member::Part {
                    data,
                    before,
                    after,
                    crc,
                } => {
                    split = if *before { 0x08 } else { 0 } | if *after { 0x10 } else { 0 };
                    (data.clone(), Vec::new(), *crc)
                }
                Member::Link { kind, target } => {
                    let mut record = Vec::new();
                    vint(0x05, &mut record); // file system redirection
                    vint(*kind, &mut record);
                    vint(0, &mut record); // flags
                    vint(target.len() as u64, &mut record);
                    record.extend_from_slice(target.as_bytes());
                    let mut extra = Vec::new();
                    vint(record.len() as u64, &mut extra);
                    extra.extend_from_slice(&record);
                    (Vec::new(), extra, 0)
                }
            };
            let mut head = Vec::new();
            vint(2, &mut head); // file header
            let mut flags = split;
            if !extra.is_empty() {
                flags |= 0x01;
            }
            if !data.is_empty() {
                flags |= 0x02;
            }
            vint(flags, &mut head);
            if !extra.is_empty() {
                vint(extra.len() as u64, &mut head);
            }
            if !data.is_empty() {
                vint(data.len() as u64, &mut head);
            }
            vint(0x04, &mut head); // file flags: CRC32 present
            vint(data.len() as u64, &mut head); // unpacked size
            vint(0o100644, &mut head); // attributes (Unix mode)
            head.extend_from_slice(&crc.to_le_bytes());
            vint(0, &mut head); // compression: version 0, stored
            vint(1, &mut head); // host OS: Unix
            vint(name.len() as u64, &mut head);
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(&extra);
            block(&mut out, &head, &data);
        }

        // End of archive: type 5, no flags, then whether another volume follows.
        let mut end = Vec::new();
        vint(5, &mut end);
        vint(0, &mut end);
        vint(u64::from(matches!(self.volume, Some((_, true)))), &mut end);
        block(&mut out, &end, &[]);
        out
    }
}

/// One block: CRC32 of (size + header), the header's size, the header, then
/// its data area.
fn block(out: &mut Vec<u8>, header: &[u8], data: &[u8]) {
    let mut sized = Vec::new();
    vint(header.len() as u64, &mut sized);
    sized.extend_from_slice(header);
    out.extend_from_slice(&crc32fast::hash(&sized).to_le_bytes());
    out.extend_from_slice(&sized);
    out.extend_from_slice(data);
}

/// RAR5's variable-length integer: 7 bits per byte, low bits first.
fn vint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
