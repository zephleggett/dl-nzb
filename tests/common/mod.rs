//! Shared test support: a tiny mock NNTP server speaking just enough NNTP to
//! satisfy the downloader (greeting, AUTHINFO, GROUP, BODY pipelined, STAT,
//! DATE, QUIT), plus yEnc/NZB builders. Articles are yEnc-encoded with `=ypart`
//! headers so the offset-aware decoder is exercised.
#![allow(dead_code)]

use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use dl_nzb::config::{Config, DownloadConfig, PostProcessingConfig, TuningConfig, UsenetConfig};
use dl_nzb::engine::{
    JobEvent, JobHandle, JobObserver, JobPhase, JobProgress, JobRequest, JobSummary, Preflight,
};

/// Pick a free port by binding to :0 in the std listener and asking the OS.
pub fn pick_free_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Build a yEnc article body for one part of a multi-part article.
pub fn build_part(
    name: &str,
    part: u32,
    total: u32,
    begin: u64,
    end: u64,
    plain: &[u8],
) -> Vec<u8> {
    yenc_part(name, (part, total), begin, end, Some(end), plain)
}

/// A one-part body for `name` placed at `begin..=end`: `=ybegin` (claiming
/// the file is `size` bytes when given), `=ypart`, `pcrc32`.
pub fn one_part(name: &str, begin: u64, end: u64, size: Option<u64>, plain: &[u8]) -> Vec<u8> {
    yenc_part(name, (1, 1), begin, end, size, plain)
}

fn yenc_part(
    name: &str,
    (part, total): (u32, u32),
    begin: u64,
    end: u64,
    size: Option<u64>,
    plain: &[u8],
) -> Vec<u8> {
    let size = size.map(|s| format!(" size={s}")).unwrap_or_default();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("=ybegin part={part} total={total} line=128{size} name={name}\r\n").as_bytes(),
    );
    body.extend_from_slice(format!("=ypart begin={begin} end={end}\r\n").as_bytes());
    body.extend_from_slice(&yenc_encode(plain));
    body.extend_from_slice(b"\r\n");
    let crc = crc32fast::hash(plain);
    body.extend_from_slice(
        format!(
            "=yend size={} part={part} pcrc32={crc:08x}\r\n",
            plain.len()
        )
        .as_bytes(),
    );
    body
}

/// A single-part body: the whole file `name`.
pub fn single(plain: &[u8], name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("=ybegin line=128 size={} name={name}\r\n", plain.len()).as_bytes(),
    );
    body.extend_from_slice(&yenc_encode(plain));
    body.extend_from_slice(b"\r\n");
    let crc = crc32fast::hash(plain);
    body.extend_from_slice(format!("=yend size={} crc32={crc:08x}\r\n", plain.len()).as_bytes());
    body
}

/// A single-part yEnc body without a `pcrc32`, so the wire can't vouch for
/// the data and PAR2 really verifies it.
pub fn article_without_crc(name: &str, plain: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "=ybegin part=1 total=1 line=128 size={} name={}\r\n",
            plain.len(),
            name
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("=ypart begin=1 end={}\r\n", plain.len()).as_bytes());
    body.extend_from_slice(&yenc_encode(plain));
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("=yend size={} part=1\r\n", plain.len()).as_bytes());
    body
}

pub fn yenc_encode(plain: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(plain.len() + plain.len() / 16 + 16);
    for &b in plain {
        let enc = b.wrapping_add(42);
        if matches!(enc, b'\0' | b'\r' | b'\n' | b'=') {
            out.push(b'=');
            out.push(enc.wrapping_add(64));
        } else {
            out.push(enc);
        }
    }
    out
}

pub struct MockArticle {
    pub message_id: String,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub struct MockServerState {
    pub articles: Vec<MockArticle>,
    /// If true, return 430 for these message ids.
    pub missing_ids: Vec<String>,
    /// Always return 412 ("no newsgroup selected") for these ids — simulates a
    /// strict/legacy server that refuses BODY-by-message-id without a group.
    pub error_412_ids: Vec<String>,
    /// Never answer BODY for these ids (the connection just stalls), to prove a
    /// stop doesn't wait out the client's read timeout.
    pub hang_ids: Vec<String>,
    /// Delay before answering each BODY, to keep a download running long
    /// enough to pause it.
    pub body_delay: Duration,
    /// Reject the password (481).
    pub reject_auth: bool,
    /// Count of BODY requests received per message id (across all connections).
    pub body_requests: std::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl MockServerState {
    pub fn body_count(&self, id: &str) -> usize {
        self.body_requests
            .lock()
            .unwrap()
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    pub fn total_body_requests(&self) -> usize {
        self.body_requests.lock().unwrap().values().sum()
    }
}

pub async fn handle_client(stream: TcpStream, state: Arc<MockServerState>) -> std::io::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);

    wr.write_all(b"200 Welcome\r\n").await?;

    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        let line = buf.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_uppercase();
        match cmd.as_str() {
            "AUTHINFO" => {
                let sub = parts.next().unwrap_or("");
                if sub.eq_ignore_ascii_case("USER") {
                    wr.write_all(b"381 More authentication required\r\n")
                        .await?;
                } else if sub.eq_ignore_ascii_case("PASS") {
                    if state.reject_auth {
                        wr.write_all(b"481 Authentication failed\r\n").await?;
                    } else {
                        wr.write_all(b"281 Authentication accepted\r\n").await?;
                    }
                } else {
                    wr.write_all(b"500 Unknown AUTHINFO\r\n").await?;
                }
            }
            "GROUP" => {
                let group = parts.next().unwrap_or("");
                wr.write_all(format!("211 1 1 1 {}\r\n", group).as_bytes())
                    .await?;
            }
            "NOOP" | "DATE" => {
                wr.write_all(b"111 20260101000000\r\n").await?;
            }
            "QUIT" => {
                wr.write_all(b"205 Bye\r\n").await?;
                return Ok(());
            }
            "STAT" => {
                let id_token = parts.next().unwrap_or("");
                let id = id_token.trim_start_matches('<').trim_end_matches('>');
                if state.missing_ids.iter().any(|m| m == id)
                    || !state.articles.iter().any(|a| a.message_id == id)
                {
                    wr.write_all(b"430 No such article\r\n").await?;
                } else {
                    wr.write_all(b"223 0 article\r\n").await?;
                }
            }
            "BODY" => {
                let id_token = parts.next().unwrap_or("");
                let id = id_token.trim_start_matches('<').trim_end_matches('>');
                if let Ok(mut counts) = state.body_requests.lock() {
                    *counts.entry(id.to_string()).or_insert(0) += 1;
                }
                if state.hang_ids.iter().any(|m| m == id) {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
                if !state.body_delay.is_zero() {
                    tokio::time::sleep(state.body_delay).await;
                }
                if state.error_412_ids.iter().any(|m| m == id) {
                    wr.write_all(b"412 No newsgroup selected\r\n").await?;
                } else if state.missing_ids.iter().any(|m| m == id) {
                    wr.write_all(b"430 No such article\r\n").await?;
                } else if let Some(article) = state.articles.iter().find(|a| a.message_id == id) {
                    wr.write_all(b"222 0 article body follows\r\n").await?;
                    wr.write_all(&article.body).await?;
                    wr.write_all(b".\r\n").await?;
                } else {
                    wr.write_all(b"430 No such article\r\n").await?;
                }
            }
            _ => {
                wr.write_all(b"500 Unknown command\r\n").await?;
            }
        }
    }
}

pub async fn start_mock_server(state: Arc<MockServerState>, port: u16, ready: Arc<Notify>) {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind mock");
    ready.notify_one();
    loop {
        let (sock, _) = match listener.accept().await {
            Ok(p) => p,
            Err(_) => return,
        };
        let s = state.clone();
        tokio::spawn(async move {
            let _ = handle_client(sock, s).await;
        });
    }
}

pub fn make_config(server: &str, port: u16, download_dir: PathBuf) -> Config {
    Config {
        usenet: UsenetConfig {
            server: server.to_string(),
            port,
            username: "user".into(),
            password: "pass".into(),
            ssl: false,
            verify_ssl_certs: false,
            connections: 4,
            retry_attempts: 2,
            retry_delay: 100,
        },
        download: DownloadConfig {
            dir: download_dir,
            create_subfolders: false,
            speed_limit: None,
        },
        post_processing: PostProcessingConfig {
            auto_par2_repair: false,
            auto_extract_rar: false,
            delete_rar_after_extract: false,
            delete_par2_after_repair: false,
            deobfuscate_file_names: false,
            download_all_par2: false,
        },
        tuning: TuningConfig {
            pipeline_depth: 4,
            decode_retry_cap: 3,
            max_concurrent_connections: 4,
            fsync_on_finalize: false,
        },
        notifications: Default::default(),
    }
}

/// [`make_config`] for the local mock server on `port`, with `connections`
/// connections (and as many opened at once).
pub fn config(port: u16, dir: &Path, connections: u16) -> Config {
    let mut config = make_config("127.0.0.1", port, dir.into());
    config.usenet.connections = connections;
    config.tuning.max_concurrent_connections = connections as usize;
    config
}

/// The server settings of [`config`].
pub fn usenet(port: u16) -> UsenetConfig {
    config(port, Path::new("."), 1).usenet
}

/// A job for `nzb` into `out` that never scans availability first.
pub fn request(nzb: &Path, out: &Path) -> JobRequest {
    JobRequest {
        preflight: Preflight::Never,
        ..JobRequest::new(nzb, out)
    }
}

/// A summary in one line, for assertion messages.
pub fn brief(s: &JobSummary) -> String {
    format!(
        "{:?} kind={:?} resumable={} failed={}/{} {:?}",
        s.outcome, s.error_kind, s.resumable, s.articles_failed, s.articles_total, s.message
    )
}

/// The names in `dir`, sorted (none when it can't be read).
pub fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

pub fn build_synthetic_nzb(
    filename: &str,
    segment_message_ids: &[String],
    segment_encoded_sizes: &[u64],
) -> String {
    nzb_xml(&[(filename, segment_message_ids, segment_encoded_sizes)])
}

/// An NZB document with the given files, each `(filename, ids, declared sizes)`.
pub fn nzb_xml(files: &[(&str, &[String], &[u64])]) -> String {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push_str(r#"<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">"#);
    for (filename, ids, sizes) in files {
        xml.push_str(&nzb_file_element(filename, ids, sizes));
    }
    xml.push_str("</nzb>");
    xml
}

/// Write `<name>.nzb` into `dir` with the given files (see [`nzb_xml`]).
pub fn write_nzb(dir: &Path, name: &str, files: &[(&str, &[String], &[u64])]) -> PathBuf {
    let path = dir.join(format!("{name}.nzb"));
    std::fs::write(&path, nzb_xml(files)).unwrap();
    path
}

/// Write `job.nzb` into `dir`: one `<file>` whose segments carry the given
/// numbers and sizes.
pub fn numbered_nzb(dir: &Path, filename: &str, segments: &[(u32, u64, String)]) -> PathBuf {
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push_str(r#"<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">"#);
    xml.push_str(&format!(
        r#"<file poster="t@e.x" date="1700000000" subject="&quot;{filename}&quot; yEnc"><groups><group>alt.binaries.test</group></groups><segments>"#
    ));
    for (number, bytes, id) in segments {
        xml.push_str(&format!(
            r#"<segment bytes="{bytes}" number="{number}">{id}</segment>"#
        ));
    }
    xml.push_str("</segments></file></nzb>");
    let path = dir.join("job.nzb");
    std::fs::write(&path, xml).unwrap();
    path
}

/// Build one `<file>` element for a multi-file synthetic NZB.
pub fn nzb_file_element(filename: &str, ids: &[String], sizes: &[u64]) -> String {
    let mut s = format!(
        r#"<file poster="t@e.x" date="1700000000" subject="[1/1] - &quot;{}&quot; yEnc (1/{})">"#,
        filename,
        ids.len()
    );
    s.push_str("<groups><group>alt.binaries.test</group></groups><segments>");
    for (i, (id, sz)) in ids.iter().zip(sizes.iter()).enumerate() {
        s.push_str(&format!(
            r#"<segment bytes="{}" number="{}">{}</segment>"#,
            sz,
            i + 1,
            id
        ));
    }
    s.push_str("</segments></file>");
    s
}

/// One single-part yEnc article of `len` deterministic bytes.
pub fn make_single_article(name: &str, id: &str, len: usize) -> (MockArticle, u64) {
    let plain: Vec<u8> = (0..len).map(|i| ((i * 13 + 5) % 256) as u8).collect();
    let body = build_part(name, 1, 1, 1, len as u64, &plain);
    (
        MockArticle {
            message_id: id.to_string(),
            body,
        },
        (len as u64) + 80,
    )
}

/// Start a mock server for `state` and return its port once it is listening.
pub async fn spawn_server(state: Arc<MockServerState>) -> u16 {
    let port = pick_free_port();
    let ready = Arc::new(Notify::new());
    let server_ready = ready.clone();
    tokio::spawn(async move { start_mock_server(state, port, server_ready).await });
    ready.notified().await;
    port
}

/// `count` articles of `segment_size` deterministic bytes for one file named
/// `filename`, with ids `{prefix}{n}@t`. Returns (articles, ids, encoded sizes,
/// the file's full plaintext).
pub fn make_file_articles(
    filename: &str,
    prefix: &str,
    count: usize,
    segment_size: usize,
) -> (Vec<MockArticle>, Vec<String>, Vec<u64>, Vec<u8>) {
    let full: Vec<u8> = (0..count * segment_size)
        .map(|i| ((i * 31 + 7) % 256) as u8)
        .collect();
    let mut articles = Vec::new();
    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    for part in 0..count {
        let begin = (part * segment_size) as u64 + 1;
        let end = ((part + 1) * segment_size) as u64;
        let plain = &full[part * segment_size..(part + 1) * segment_size];
        let body = build_part(filename, (part + 1) as u32, count as u32, begin, end, plain);
        let id = format!("{prefix}{}@t", part + 1);
        ids.push(id.clone());
        sizes.push(segment_size as u64 + 64);
        articles.push(MockArticle {
            message_id: id,
            body,
        });
    }
    (articles, ids, sizes, full)
}

/// Wait (up to 20 s) until `done`.
pub async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Close `sock` with a reset (RST) rather than an orderly FIN.
pub fn reset(sock: TcpStream) {
    socket2::SockRef::from(&sock)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(sock);
}

/// Wait for a job, failing the test if it takes longer than `secs`.
pub async fn wait(job: &JobHandle, secs: u64) -> JobSummary {
    tokio::time::timeout(Duration::from_secs(secs), job.wait())
        .await
        .expect("job did not finish in time")
}

/// Records every event in order.
#[derive(Default)]
pub struct Recorder {
    events: Mutex<Vec<JobEvent>>,
}

impl JobObserver for Recorder {
    fn on_event(&self, event: JobEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl Recorder {
    pub fn events(&self) -> Vec<JobEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn phases(&self) -> Vec<JobPhase> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                JobEvent::Phase(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    pub fn warnings(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                JobEvent::Warning(w) => Some(w),
                _ => None,
            })
            .collect()
    }

    /// The progress sent with `Phase(phase)` (the phase's starting point).
    pub fn first_progress(&self, phase: JobPhase) -> Option<JobProgress> {
        let events = self.events();
        let at = events
            .iter()
            .position(|e| matches!(e, JobEvent::Phase(p) if *p == phase))?;
        match events.get(at + 1) {
            Some(JobEvent::Progress(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Wait (up to 20 s) until some recorded event satisfies `pred`.
    pub async fn wait_for(&self, pred: impl Fn(&JobEvent) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.events.lock().unwrap().iter().any(&pred) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for an event; got {:?}", self.phases());
    }
}

// Tiny RAR fixtures from the `unrar` crate's test data (MIT/Apache-2.0).

/// `version.rar`: one plain member, `VERSION`.
pub const PLAIN_RAR: &[u8] = &[
    0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0xcf, 0x90, 0x73, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x0f, 0x0c, 0x74, 0x20, 0x80, 0x27, 0x00, 0x15, 0x00, 0x00, 0x00, 0x0b,
    0x00, 0x00, 0x00, 0x03, 0x45, 0xf3, 0x7d, 0xc6, 0xa4, 0x8a, 0x07, 0x47, 0x1d, 0x33, 0x07, 0x00,
    0xa4, 0x81, 0x00, 0x00, 0x56, 0x45, 0x52, 0x53, 0x49, 0x4f, 0x4e, 0x0c, 0x00, 0x8f, 0xec, 0x8a,
    0x45, 0xcc, 0x23, 0xc8, 0x48, 0x08, 0x83, 0x62, 0xfe, 0x5f, 0xdd, 0x5c, 0x53, 0x88, 0xf0, 0x72,
    0xc4, 0x3d, 0x7b, 0x00, 0x40, 0x07, 0x00,
];
/// `crypted.rar`: RAR 2.9, one member (`.gitignore`) whose data is encrypted
/// with `unrar`.
pub const CRYPTED_RAR: &[u8] = &[
    0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0xcf, 0x90, 0x73, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xd3, 0xd9, 0x74, 0x24, 0x84, 0x32, 0x00, 0x20, 0x00, 0x00, 0x00, 0x12,
    0x00, 0x00, 0x00, 0x03, 0xf3, 0x8a, 0x03, 0x6e, 0x2d, 0x81, 0x03, 0x47, 0x1d, 0x33, 0x0a, 0x00,
    0xa4, 0x81, 0x00, 0x00, 0x2e, 0x67, 0x69, 0x74, 0x69, 0x67, 0x6e, 0x6f, 0x72, 0x65, 0x89, 0x04,
    0xba, 0x8c, 0x93, 0x06, 0x43, 0x22, 0x1f, 0x39, 0x85, 0xf9, 0x6f, 0x25, 0x5f, 0x39, 0xcf, 0xe9,
    0x21, 0x24, 0x06, 0x56, 0x3c, 0x12, 0x4f, 0x90, 0x06, 0xca, 0xfc, 0xd9, 0x62, 0xd8, 0x5f, 0xf0,
    0xc7, 0x23, 0x32, 0xa5, 0x2e, 0x6d, 0xc4, 0x3d, 0x7b, 0x00, 0x40, 0x07, 0x00,
];
/// `comment-hpw-password.rar`: RAR5, headers (and data) encrypted with
/// `password`; one member, `.gitignore`.
pub const HEADER_ENCRYPTED_RAR: &[u8] = &[
    0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00, 0x9b, 0xf5, 0x3c, 0x33, 0x21, 0x04, 0x00, 0x00,
    0x01, 0x0f, 0x60, 0x69, 0x36, 0x3a, 0x0a, 0x3b, 0xe9, 0x1b, 0x95, 0x56, 0xe8, 0xf0, 0xc9, 0x6f,
    0x70, 0xde, 0x59, 0x54, 0xf8, 0x8a, 0x85, 0xed, 0xea, 0x85, 0xbb, 0x95, 0x1d, 0xf1, 0x3f, 0x54,
    0x41, 0xb4, 0x47, 0xd3, 0x3b, 0x7d, 0xb9, 0x55, 0x08, 0xab, 0x61, 0x05, 0xfd, 0x38, 0x57, 0xbe,
    0x32, 0xaf, 0x29, 0x17, 0xc5, 0x95, 0x60, 0xfa, 0xf2, 0x37, 0xcb, 0xbf, 0x9a, 0x70, 0x01, 0x20,
    0x29, 0x83, 0x07, 0xce, 0x47, 0x4b, 0xa6, 0xc7, 0xf6, 0x83, 0x99, 0x49, 0x65, 0x3f, 0x41, 0x87,
    0x5c, 0x50, 0x05, 0x2a, 0xbe, 0x2c, 0xe6, 0xd0, 0x26, 0xaa, 0x3a, 0x5f, 0x77, 0xad, 0x01, 0x3a,
    0x52, 0x64, 0x72, 0x27, 0x49, 0x92, 0x72, 0x80, 0xf0, 0xa8, 0x86, 0x4d, 0xa3, 0x51, 0x9b, 0xd9,
    0x56, 0x01, 0x7e, 0xc2, 0xa7, 0x5d, 0x5f, 0xa7, 0x57, 0x4b, 0xf7, 0xc2, 0x47, 0x11, 0x1a, 0x7d,
    0xad, 0xf3, 0x3d, 0x7d, 0xd5, 0x4d, 0x0b, 0xbc, 0xad, 0x48, 0x42, 0xee, 0xbf, 0x4b, 0x5e, 0x46,
    0xef, 0xc5, 0x01, 0xf3, 0x26, 0xab, 0xcf, 0x15, 0x77, 0xf5, 0xd6, 0xef, 0x03, 0x31, 0x43, 0x36,
    0xa7, 0xbc, 0xc3, 0xa3, 0x6d, 0x64, 0xef, 0xc5, 0x15, 0x7e, 0xe9, 0xc4, 0xa3, 0x3b, 0xa9, 0x89,
    0xa7, 0xed, 0x57, 0x7e, 0x7d, 0x51, 0x52, 0x97, 0xf3, 0xe6, 0xe7, 0x78, 0x59, 0xb6, 0xf2, 0x05,
    0x81, 0x5b, 0x16, 0x93, 0x05, 0x49, 0xc2, 0x6e, 0x04, 0x74, 0x62, 0xde, 0x39, 0xee, 0x9f, 0x81,
    0x60, 0x60, 0x3b, 0x78, 0x30, 0xd2, 0x9d, 0x1a, 0x6a, 0xcc, 0x9e, 0xc8, 0xa0, 0xab, 0xa9, 0xf1,
    0x86, 0x07, 0x1d, 0xd1, 0x51, 0xa2, 0xba, 0xb7, 0xc9, 0x03, 0x5f, 0x21, 0x41, 0xca, 0xbe, 0x45,
    0xc7, 0x3e, 0x8d, 0xf8, 0x1a, 0x00, 0x90, 0x3d, 0x72, 0x66, 0x95, 0xf1, 0x26, 0x66, 0xfd, 0xdc,
    0x5d, 0xf0, 0x79, 0x72, 0x8c, 0x1d, 0xbb, 0x59, 0x73, 0x09, 0xf9, 0xc5, 0x63, 0x5c, 0x9a, 0x27,
    0x0c, 0x37, 0xc7, 0xf0, 0xb1, 0xf8, 0xd7, 0x8f, 0xb0, 0x35, 0x4a, 0xf6, 0x02, 0x50, 0xf0, 0x9b,
    0xe6, 0x1b, 0xa9, 0x93, 0x89, 0x59, 0x75, 0x21, 0xe0, 0x95, 0xfb, 0xca, 0x8c, 0x8b, 0x59, 0xab,
    0x43, 0x3a, 0xef, 0x8d, 0x83, 0xbf, 0xaa, 0x5c, 0x13, 0x34, 0x7a, 0x69, 0x35, 0x62, 0xc1, 0x1d,
    0x15, 0x1b, 0xa8, 0x00, 0xc3, 0x8a, 0x73, 0x49, 0xb3, 0xce, 0xd5, 0xba, 0x23, 0x0f, 0x87, 0xf5,
    0x40, 0xda, 0x6b, 0x3c, 0xbb, 0x58, 0x66, 0x18, 0x0a, 0x15, 0x86, 0x6e, 0x4d, 0x57,
];
