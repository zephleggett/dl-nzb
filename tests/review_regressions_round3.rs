//! Regressions from the third verification pass: where an article may place
//! its part (a limit set by the NZB alone), PAR2's verdict surviving a stop
//! while files are renamed, an article that always resets the connection,
//! `reprocess` of a download that never finished, invisible characters in a
//! title, SSL on a plaintext port, and "too many connections" at login.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, ErrorKind, JobEvent, JobHandle, JobObserver, JobPhase, JobRequest, Outcome, Preflight,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const SIDECAR: &str = ".dl-nzb-job.json";
const MIB: u64 = 1 << 20;

fn config(port: u16, dir: &Path, connections: u16) -> dl_nzb::Config {
    let mut config = make_config("127.0.0.1", port, dir.into());
    config.usenet.connections = connections;
    config.tuning.max_concurrent_connections = connections as usize;
    config
}

fn request(nzb: &Path, out: &Path) -> JobRequest {
    JobRequest {
        preflight: Preflight::Never,
        ..JobRequest::new(nzb, out)
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn brief(s: &dl_nzb::JobSummary) -> String {
    format!(
        "{:?} kind={:?} resumable={} failed={}/{} {:?}",
        s.outcome, s.error_kind, s.resumable, s.articles_failed, s.articles_total, s.message
    )
}

// --- A scriptable server -------------------------------------------------------------

/// What the server does with a `BODY` for an article.
#[derive(Clone)]
enum Act {
    Serve(Vec<u8>),
    Missing,
    /// "412 no newsgroup selected", every time.
    Refuse412,
    /// Starts the body, then resets the connection.
    Reset,
}

#[derive(Default)]
struct Srv {
    acts: Mutex<HashMap<String, Act>>,
    bodies: Mutex<HashMap<String, usize>>,
    accepted: AtomicUsize,
}

impl Srv {
    fn set(&self, id: &str, act: Act) {
        self.acts.lock().unwrap().insert(id.to_string(), act);
    }

    fn bodies_for(&self, id: &str) -> usize {
        self.bodies.lock().unwrap().get(id).copied().unwrap_or(0)
    }
}

/// Close `sock` with a reset (RST) rather than an orderly FIN.
fn reset(sock: TcpStream) {
    socket2::SockRef::from(&sock)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(sock);
}

async fn session(sock: TcpStream, srv: Arc<Srv>) {
    let (rd, mut wr) = sock.into_split();
    let _ = wr.write_all(b"200 hi\r\n").await;
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("AUTHINFO USER") {
            let _ = wr.write_all(b"381 more\r\n").await;
        } else if upper.starts_with("AUTHINFO PASS") {
            let _ = wr.write_all(b"281 ok\r\n").await;
        } else if upper.starts_with("GROUP") {
            let _ = wr.write_all(b"211 1 1 1 g\r\n").await;
        } else if upper.starts_with("QUIT") {
            let _ = wr.write_all(b"205 bye\r\n").await;
            return;
        } else if upper.starts_with("BODY") {
            let id = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            *srv.bodies.lock().unwrap().entry(id.clone()).or_default() += 1;
            let act = srv.acts.lock().unwrap().get(&id).cloned();
            match act {
                Some(Act::Serve(body)) => {
                    let _ = wr.write_all(b"222 0 body\r\n").await;
                    let _ = wr.write_all(&body).await;
                    let _ = wr.write_all(b".\r\n").await;
                }
                Some(Act::Refuse412) => {
                    let _ = wr.write_all(b"412 no group\r\n").await;
                }
                Some(Act::Reset) => {
                    let _ = wr
                        .write_all(b"222 0 body\r\n=ybegin part=1 line=128 size=1000 name=x\r\n")
                        .await;
                    // Closed (FIN) before the reset: macOS can ignore a
                    // reset that arrives right behind data it hasn't
                    // acknowledged yet (it answers with a "challenge ACK",
                    // RFC 5961, at most a few a second), and the client
                    // would then wait out its read timeout. The close
                    // always gets there.
                    let _ = wr.shutdown().await;
                    reset(reader.into_inner().reunite(wr).unwrap());
                    return;
                }
                _ => {
                    let _ = wr.write_all(b"430 no\r\n").await;
                }
            }
        } else {
            let _ = wr.write_all(b"111 20260101000000\r\n").await;
        }
    }
}

async fn serve(srv: Arc<Srv>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            srv.accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(session(sock, srv.clone()));
        }
    });
    port
}

// --- yEnc and NZB builders -----------------------------------------------------------

/// A one-part body placed at `begin..=end`, claiming the file is `size`
/// bytes when given.
fn part(begin: u64, end: u64, size: Option<u64>, plain: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    let size = size.map(|s| format!(" size={s}")).unwrap_or_default();
    body.extend_from_slice(
        format!("=ybegin part=1 total=1 line=128{size} name=f.bin\r\n").as_bytes(),
    );
    body.extend_from_slice(format!("=ypart begin={begin} end={end}\r\n").as_bytes());
    body.extend_from_slice(&yenc_encode(plain));
    body.extend_from_slice(b"\r\n");
    let crc = crc32fast::hash(plain);
    body.extend_from_slice(
        format!("=yend size={} part=1 pcrc32={crc:08x}\r\n", plain.len()).as_bytes(),
    );
    body
}

/// A single-part body: the whole file `name`.
fn single(plain: &[u8], name: &str) -> Vec<u8> {
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

/// An NZB with one `<file>` whose segments carry the given numbers and sizes.
fn numbered_nzb(dir: &Path, filename: &str, segments: &[(u32, u64, String)]) -> PathBuf {
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

/// Download `f.bin` as listed by `segments`: the summary, and the file as it
/// ended up (`f.bin`, else `f.bin.partial`; empty when neither exists).
async fn download_f_bin(
    srv: Arc<Srv>,
    segments: &[(u32, u64, String)],
) -> (dl_nzb::JobSummary, u64, Vec<u8>) {
    let port = serve(srv).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = numbered_nzb(temp.path(), "f.bin", segments);
    let out = temp.path().join("Job");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    let path = ["f.bin", "f.bin.partial"]
        .iter()
        .map(|n| out.join(n))
        .find(|p| p.exists());
    let (len, data) = match path {
        Some(p) => {
            let len = std::fs::metadata(&p).unwrap().len();
            let data = if len <= 64 * MIB {
                std::fs::read(&p).unwrap()
            } else {
                Vec::new()
            };
            (len, data)
        }
        None => (0, Vec::new()),
    };
    (summary, len, data)
}

// --- 1. Placement ----------------------------------------------------------------------

/// NZB sizes are unreliable: under-reported (here 10 times), absent, the
/// decoded rather than the encoded size, or an entry missing. Every honest
/// part must still land, with or without a `=ybegin size=`. The old limit
/// grew with the parts seen, but only up to 4x the NZB's largest size, so a
/// release whose NZB said 10x too little failed (as did one saying 100
/// bytes, or 0 everywhere but one entry).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn honest_parts_land_whatever_sizes_the_nzb_gives() {
    let (count, seg) = (10usize, 50_000usize);
    let total = (count * seg) as u64;
    let full: Vec<u8> = (0..count * seg)
        .map(|i| ((i * 7 + 3) % 251) as u8)
        .collect();
    type Declared = Box<dyn Fn(usize) -> u64>;
    let cases: Vec<(&str, bool, Declared, Option<usize>)> = vec![
        ("10x under-reported", true, Box::new(|_| 5_000), None),
        (
            "6x under-reported, no size=",
            false,
            Box::new(|_| 8_333),
            None,
        ),
        ("100 bytes each", true, Box::new(|_| 100), None),
        (
            "0 but one 500",
            true,
            Box::new(|k| if k == 9 { 500 } else { 0 }),
            None,
        ),
        ("0 everywhere, no size=", false, Box::new(|_| 0), None),
        ("entry 5 missing", true, Box::new(|_| 51_500), Some(4)),
        ("decoded sizes", true, Box::new(|_| 50_000), None),
    ];
    let mut wrong = Vec::new();
    for (name, claim, declared, skip) in cases {
        let srv = Arc::new(Srv::default());
        let mut segments = Vec::new();
        for k in 0..count {
            let id = format!("p{k}@t");
            srv.set(
                &id,
                Act::Serve(part(
                    (k * seg + 1) as u64,
                    ((k + 1) * seg) as u64,
                    claim.then_some(total),
                    &full[k * seg..(k + 1) * seg],
                )),
            );
            if Some(k) != skip {
                segments.push(((k + 1) as u32, declared(k), id));
            }
        }
        let (summary, len, data) = download_f_bin(srv, &segments).await;
        let landed = (0..count)
            .filter(|&k| Some(k) != skip)
            .all(|k| data.get(k * seg..(k + 1) * seg) == Some(&full[k * seg..(k + 1) * seg]));
        if summary.outcome != Outcome::Completed || !landed || len != total {
            wrong.push(format!("{name}: {} len={len}", brief(&summary)));
        }
    }

    // One part, the whole file of 50 KB, that the NZB says is 100 bytes.
    let srv = Arc::new(Srv::default());
    let plain: Vec<u8> = (0..50_000).map(|i| (i % 253) as u8).collect();
    srv.set("one@t", Act::Serve(single(&plain, "f.bin")));
    let (summary, _, data) = download_f_bin(srv, &[(1, 100, "one@t".into())]).await;
    if summary.outcome != Outcome::Completed || data != plain {
        wrong.push(format!("single part, NZB says 100: {}", brief(&summary)));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The limit comes from the NZB alone: max(16 x its sizes, 4 MiB per part
/// number) + 4 MiB. A part may end exactly there, never a byte past it,
/// whatever its article claims the file's size is. (The old limit was a few
/// KB here, and a `size=` claim could double it.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_part_may_reach_the_limit_the_nzb_sets_and_no_further() {
    // Two parts numbered 1 and 2, no sizes: limit = 2 x 4 MiB + 4 MiB.
    let limit = 12 * MIB;
    let honest: Vec<u8> = (0..1000).map(|i| (i % 249) as u8).collect();
    let tail: Vec<u8> = (0..1000).map(|i| (i % 241) as u8).collect();
    let layout = |end: u64, claim: Option<u64>| {
        let srv = Arc::new(Srv::default());
        srv.set("h@t", Act::Serve(part(1, 1000, None, &honest)));
        srv.set("c@t", Act::Serve(part(end - 999, end, claim, &tail)));
        let segments = vec![(1, 0, "h@t".to_string()), (2, 0, "c@t".to_string())];
        (srv, segments)
    };

    let (srv, segments) = layout(limit, None);
    let (summary, len, data) = download_f_bin(srv, &segments).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{}", brief(&summary));
    assert_eq!(len, limit);
    assert_eq!(&data[..1000], &honest[..]);
    assert_eq!(&data[(limit - 1000) as usize..], &tail[..]);

    for claim in [None, Some(100 * MIB)] {
        let (srv, segments) = layout(limit + 1, claim);
        let (summary, len, data) = download_f_bin(srv, &segments).await;
        assert_eq!(summary.articles_failed, 1, "{}", brief(&summary));
        assert_ne!(summary.outcome, Outcome::Completed);
        assert_eq!(len, 1000, "claim {claim:?}");
        assert_eq!(data, honest);
    }
}

// --- 2. A stop while renaming, after PAR2 repaired and its files were deleted ----------

/// Stops the job, from inside the observer, as `phase` begins.
struct StopAtPhase {
    phase: JobPhase,
    job: OnceLock<JobHandle>,
}

impl JobObserver for StopAtPhase {
    fn on_event(&self, event: JobEvent) {
        if event != JobEvent::Phase(self.phase) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.job.get().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.job.get().expect("job handle").stop();
    }
}

/// A release PAR2 repairs (A.rar is missing), whose PAR2 files are deleted
/// after the repair and whose B.bin is renamed after the job ("job.bin").
/// The job is stopped as renaming begins (the rename still happens). The
/// saved PAR2 verdict named B.bin, so a resume found it "changed", wanted
/// PAR2 again, and failed: "12% of articles are missing and the PAR2 files
/// needed to repair them are gone". Renames now carry the verdict along.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_while_renaming_keeps_par2s_verdict() {
    let big: Vec<u8> = (0..40_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let source = tempfile::tempdir().unwrap();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("A.rar", PLAIN_RAR.to_vec()),
        ("B.bin", big.clone()),
        ("C.rar", PLAIN_RAR.to_vec()),
    ];
    let mut paths = Vec::new();
    for (name, bytes) in &files {
        std::fs::write(source.path().join(name), bytes).unwrap();
        paths.push(source.path().join(name));
    }
    let par2 = par2_rs::Par2Creator::new(paths)
        .unwrap()
        .with_block_size(2048)
        .unwrap()
        .with_redundancy(60.0)
        .unwrap()
        .create()
        .unwrap();
    let mut posted: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, b)| (n.to_string(), b.clone()))
        .collect();
    for p in &par2 {
        posted.push((
            p.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::read(p).unwrap(),
        ));
    }
    let srv = Arc::new(Srv::default());
    let mut listed: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
    for (i, (name, bytes)) in posted.iter().enumerate() {
        let id = format!("f{i}@t");
        srv.set(&id, Act::Serve(single(bytes, name)));
        listed.push((name.clone(), vec![id], vec![bytes.len() as u64 + 64]));
    }
    srv.set("f0@t", Act::Missing); // A.rar: PAR2 rebuilds it
    let port = serve(srv).await;

    for (reprocess, delete_rar) in [(false, false), (true, false), (false, true), (true, true)] {
        let temp = tempfile::tempdir().unwrap();
        let refs: Vec<(&str, &[String], &[u64])> = listed
            .iter()
            .map(|(n, i, s)| (n.as_str(), i.as_slice(), s.as_slice()))
            .collect();
        let nzb = write_nzb(temp.path(), "job", &refs);
        let out = temp.path().join("Job");
        let mut cfg = config(port, temp.path(), 2);
        cfg.post_processing.auto_par2_repair = true;
        cfg.post_processing.auto_extract_rar = true;
        cfg.post_processing.delete_par2_after_repair = true;
        cfg.post_processing.delete_rar_after_extract = delete_rar;
        cfg.post_processing.deobfuscate_file_names = true;
        let engine = Engine::new(cfg).unwrap();
        let observer = Arc::new(StopAtPhase {
            phase: JobPhase::Renaming,
            job: OnceLock::new(),
        });
        let job = engine.start(request(&nzb, &out), observer.clone());
        let _ = observer.job.set(job.clone());
        let first = wait(&job, 30).await;
        assert_eq!(first.outcome, Outcome::Stopped, "{}", brief(&first));
        assert!(first.resumable && out.join(SIDECAR).exists());
        assert!(out.join("job.bin").exists(), "{:?}", names_in(&out));

        let second = if reprocess {
            wait(
                &engine.reprocess(out.clone(), Vec::new(), Arc::new(Recorder::default())),
                30,
            )
            .await
        } else {
            wait(
                &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
                30,
            )
            .await
        };
        assert_eq!(
            second.outcome,
            Outcome::Completed,
            "reprocess={reprocess} delete_rar={delete_rar}: {} {:?}",
            brief(&second),
            names_in(&out)
        );
        assert!(!out.join(SIDECAR).exists());
        assert!(out.join("VERSION").exists());
        assert_eq!(std::fs::read(out.join("job.bin")).unwrap(), big);
    }
}

// --- 3. An article that always resets the connection ----------------------------------

/// `count` 1,000-byte articles of x.bin on a scriptable server, every one
/// served; returns the server, ids, bodies and the file.
fn x_bin(count: usize) -> (Arc<Srv>, Vec<String>, Vec<Vec<u8>>, Vec<u8>) {
    let (articles, ids, _, full) = make_file_articles("x.bin", "x", count, 1_000);
    let srv = Arc::new(Srv::default());
    let bodies = articles.iter().map(|a| a.body.clone()).collect();
    for a in articles {
        srv.set(&a.message_id, Act::Serve(a.body));
    }
    (srv, ids, bodies, full)
}

async fn run_x(srv: Arc<Srv>, temp: &Path, ids: &[String], retry: u8) -> dl_nzb::JobSummary {
    let nzb = write_nzb(temp, "x", &[("x.bin", ids, &vec![1064; ids.len()])]);
    let port = serve(srv).await;
    let mut cfg = config(port, temp, 2);
    cfg.usenet.retry_attempts = retry;
    let engine = Engine::new(cfg).unwrap();
    wait(
        &engine.start(
            request(&nzb, &temp.join("X")),
            Arc::new(Recorder::default()),
        ),
        60,
    )
    .await
}

/// A release PAR2 can repair: B.bin in 30 parts, and A.bin one article that
/// always resets the connection. That failed every time as "The connection
/// kept dropping" (resumable), so the job never got to PAR2. The article is
/// now given up as missing after a few tries on its own, and PAR2 rebuilds
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_article_that_always_resets_is_left_to_par2() {
    let source = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..60_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let small: Vec<u8> = (0..8_000u32).map(|i| (i * 3 % 241) as u8).collect();
    std::fs::write(source.path().join("B.bin"), &big).unwrap();
    std::fs::write(source.path().join("A.bin"), &small).unwrap();
    let par2 = par2_rs::Par2Creator::new(vec![
        source.path().join("A.bin"),
        source.path().join("B.bin"),
    ])
    .unwrap()
    .with_block_size(2048)
    .unwrap()
    .with_redundancy(30.0)
    .unwrap()
    .create()
    .unwrap();
    let srv = Arc::new(Srv::default());
    let mut listed: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
    srv.set("a@t", Act::Reset);
    listed.push(("A.bin".into(), vec!["a@t".into()], vec![8_064]));
    let mut b_ids = Vec::new();
    for k in 0..30 {
        let id = format!("b{k}@t");
        srv.set(
            &id,
            Act::Serve(part(
                (k * 2000 + 1) as u64,
                ((k + 1) * 2000) as u64,
                Some(60_000),
                &big[k * 2000..(k + 1) * 2000],
            )),
        );
        b_ids.push(id);
    }
    listed.push(("B.bin".into(), b_ids, vec![2_064; 30]));
    for (i, p) in par2.iter().enumerate() {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(p).unwrap();
        let id = format!("p{i}@t");
        srv.set(&id, Act::Serve(single(&bytes, &name)));
        listed.push((name, vec![id], vec![bytes.len() as u64 + 64]));
    }
    let temp = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, &[String], &[u64])> = listed
        .iter()
        .map(|(n, i, s)| (n.as_str(), i.as_slice(), s.as_slice()))
        .collect();
    let nzb = write_nzb(temp.path(), "job", &refs);
    let out = temp.path().join("Job");
    let port = serve(srv.clone()).await;
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.retry_attempts = 2;
    cfg.post_processing.auto_par2_repair = true;
    let engine = Engine::new(cfg).unwrap();
    let summary = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Completed, "{}", brief(&summary));
    assert!(summary.par2.repaired, "{:?}", summary.par2);
    assert_eq!(std::fs::read(out.join("A.bin")).unwrap(), small);
    assert_eq!(std::fs::read(out.join("B.bin")).unwrap(), big);
    // Tried more than once before it was given up.
    assert!(srv.bodies_for("a@t") >= 3, "{}", srv.bodies_for("a@t"));
}

/// Articles that shared a window with the one that reset the connection
/// aren't its fault: they are fetched again, never given up with it. All
/// four articles used to fail ("kept dropping"). With no PAR2 to repair it,
/// the one article that kept dropping the connection leaves the job failed
/// as a connection problem, resumably, rather than "missing".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn articles_beside_a_reset_are_not_blamed_for_it() {
    for count in [4usize, 8] {
        let (srv, ids, _, full) = x_bin(count);
        srv.set(&ids[0], Act::Reset);
        let temp = tempfile::tempdir().unwrap();
        let summary = run_x(srv, temp.path(), &ids, 1).await;
        assert_eq!(summary.articles_failed, 1, "{}", brief(&summary));
        assert_eq!(summary.outcome, Outcome::Failed);
        assert_eq!(
            summary.error_kind,
            Some(ErrorKind::Connect),
            "{}",
            brief(&summary)
        );
        assert!(summary.resumable, "{}", brief(&summary));
        let got = std::fs::read(temp.path().join("X").join("x.bin")).unwrap();
        assert_eq!(got[1000..], full[1000..]);
    }
}

/// "412 no newsgroup selected" refusals that shared a window with a reset
/// are refusals, not lost connections, and the healthy articles beside them
/// land: 1 missing + 1 reset + 3 refused of 8 is 5 failed, not "The
/// connection kept dropping" with all 8 failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusals_beside_a_reset_are_not_lost_connections() {
    for depth in [1usize, 4] {
        let (srv, ids, _, full) = x_bin(8);
        srv.set(&ids[0], Act::Missing);
        srv.set(&ids[1], Act::Reset);
        for id in &ids[2..5] {
            srv.set(id, Act::Refuse412);
        }
        let temp = tempfile::tempdir().unwrap();
        let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &[1064; 8])]);
        let port = serve(srv).await;
        let mut cfg = config(port, temp.path(), 2);
        cfg.usenet.retry_attempts = 1;
        cfg.tuning.pipeline_depth = depth;
        let engine = Engine::new(cfg).unwrap();
        let summary = wait(
            &engine.start(
                request(&nzb, &temp.path().join("X")),
                Arc::new(Recorder::default()),
            ),
            60,
        )
        .await;
        assert_eq!(
            summary.articles_failed,
            5,
            "depth {depth}: {}",
            brief(&summary)
        );
        assert_eq!(
            summary.error_kind,
            None,
            "depth {depth}: {}",
            brief(&summary)
        );
        assert_eq!(
            summary.message.as_deref(),
            Some("62% of articles are missing and there is no recovery data to repair them.")
        );
        let got = std::fs::read(temp.path().join("X").join("x.bin")).unwrap();
        assert_eq!(got[5000..], full[5000..]);
    }
}

/// 3 missing articles and 1 that resets the connection: the reset one is
/// tried again (on its own) before the job is judged, and the job is judged
/// the same way when started again (it said 75% missing, then 100%).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_beside_missing_articles_is_judged_the_same_on_resume() {
    let (srv, ids, bodies, _) = x_bin(4);
    for id in &ids[..3] {
        srv.set(id, Act::Missing);
    }
    srv.set(&ids[3], Act::Reset);
    let temp = tempfile::tempdir().unwrap();
    let first = run_x(srv.clone(), temp.path(), &ids, 1).await;
    assert!(srv.bodies_for(&ids[3]) >= 3, "{}", srv.bodies_for(&ids[3]));
    assert_eq!(first.outcome, Outcome::Failed, "{}", brief(&first));
    assert_eq!(first.error_kind, None, "{}", brief(&first));
    // The server then serves it: the job's verdict stands.
    srv.set(&ids[3], Act::Serve(bodies[3].clone()));
    let second = run_x(srv, temp.path(), &ids, 1).await;
    assert_eq!(second.outcome, Outcome::Failed, "{}", brief(&second));
    assert_eq!(
        first.message,
        second.message,
        "{} / {}",
        brief(&first),
        brief(&second)
    );
    assert_eq!(first.articles_failed, second.articles_failed);
}

// --- 4. reprocess of a download that never finished ------------------------------------

/// A.bin finished, B.bin was still downloading when the job was stopped
/// (B.bin.partial). `reprocess` called that Completed and deleted the
/// sidecar, though B.bin never arrived. It is a hole: not Completed, and the
/// sidecar stays so `start()` can finish the download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reprocess_of_a_stopped_download_is_not_completed() {
    let (mut articles, a_ids, a_sizes, _) = make_file_articles("A.bin", "a", 2, 1_000);
    let (b_articles, b_ids, b_sizes, _) = make_file_articles("B.bin", "b", 1, 1_000);
    articles.extend(b_articles);
    let state = Arc::new(MockServerState {
        articles,
        hang_ids: b_ids.clone(),
        ..Default::default()
    });
    let port = spawn_server(state).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(
        temp.path(),
        "x",
        &[("A.bin", &a_ids, &a_sizes), ("B.bin", &b_ids, &b_sizes)],
    );
    let out = temp.path().join("X");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let job = engine.start(request(&nzb, &out), Arc::new(Recorder::default()));
    let t = Instant::now();
    while !out.join("A.bin").exists() && t.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    job.stop();
    let first = wait(&job, 30).await;
    assert_eq!(first.outcome, Outcome::Stopped, "{}", brief(&first));
    assert!(out.join("B.bin.partial").exists(), "{:?}", names_in(&out));

    let again = wait(
        &engine.reprocess(out.clone(), Vec::new(), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_ne!(again.outcome, Outcome::Completed, "{}", brief(&again));
    assert!(again.articles_failed >= 1, "{}", brief(&again));
    assert!(again.resumable, "{}", brief(&again));
    assert!(out.join(SIDECAR).exists());

    // A `.partial` with no record at all is a hole too.
    std::fs::remove_file(out.join(SIDECAR)).unwrap();
    let bare = wait(
        &engine.reprocess(out.clone(), Vec::new(), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_ne!(bare.outcome, Outcome::Completed, "{}", brief(&bare));
}

// --- 5. Invisible characters in a title --------------------------------------------------

const SCRAMBLED: &str = "a1b2c3d4e5f6a7b8c9d0.mkv";

/// The `.mkv` files a scrambled download from `Name.nzb` ends with, renamed
/// after `title`.
async fn renamed_with(title: &str) -> Vec<String> {
    let (articles, ids, sizes, _) = make_file_articles(SCRAMBLED, "o", 2, 30_000);
    let port = spawn_server(Arc::new(MockServerState {
        articles,
        ..Default::default()
    }))
    .await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "Name", &[(SCRAMBLED, &ids, &sizes)]);
    let out = temp.path().join("Name 2");
    let mut cfg = config(port, temp.path(), 2);
    cfg.post_processing.deobfuscate_file_names = true;
    let engine = Engine::new(cfg).unwrap();
    let req = JobRequest {
        title: Some(title.to_string()),
        ..request(&nzb, &out)
    };
    let summary = wait(&engine.start(req, Arc::new(Recorder::default())), 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    names_in(&out)
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".mkv"))
        .collect()
}

/// Invisible format characters (zero-width space, BOM, word joiner, zero-
/// width joiner, soft hyphen) and fillers (Hangul filler) never reach a
/// file name: a title made only of them falls back to the NZB's name ("\u{200B}.mkv"
/// was the file's name), and one hiding the extension is still not given
/// it twice ("Name.mkv\u{200B}.mkv").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invisible_characters_never_reach_a_file_name() {
    for (title, want) in [
        ("\u{200B}", "Name.mkv"),
        ("\u{FEFF}", "Name.mkv"),
        ("\u{2060}", "Name.mkv"),
        ("\u{200D}", "Name.mkv"),
        ("\u{00AD}", "Name.mkv"),
        ("\u{3164}", "Name.mkv"),
        ("Name.mkv\u{200B}", "Name.mkv"),
        ("Big\u{200B} Mo\u{FEFF}vie\u{3164}", "Big Movie.mkv"),
    ] {
        assert_eq!(renamed_with(title).await, vec![want], "title {title:?}");
    }
}

// --- 6. SSL on a plaintext port, "too many connections" at login ------------------------

/// A plain NNTP server: greets, then holds the connection.
async fn plain_greeter(accepted: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = sock.write_all(b"200 news.example.com ready\r\n").await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            });
        }
    });
    port
}

/// SSL turned on against a port that speaks plain NNTP: the server greets
/// where the TLS handshake expects a record. That read as a lost connection
/// (`Connect`), asked again 3 times over several seconds. It is a TLS
/// failure that says what is wrong, at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssl_on_a_plaintext_port_says_so_at_once() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let port = plain_greeter(accepted.clone()).await;
    let mut usenet = make_config("127.0.0.1", port, ".".into()).usenet;
    usenet.ssl = true;
    let err = Engine::test_connection(&usenet).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Tls, "{err}");
    assert_eq!(
        err.user_message(),
        "The server doesn't use SSL on this port."
    );

    let accepted = Arc::new(AtomicUsize::new(0));
    let port = plain_greeter(accepted.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let (_, ids, sizes, _) = make_file_articles("doc.bin", "d", 2, 1_000);
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.ssl = true;
    cfg.usenet.retry_attempts = 3;
    cfg.usenet.retry_delay = 300;
    let engine = Engine::new(cfg).unwrap();
    let started = Instant::now();
    let summary = wait(
        &engine.start(
            request(&nzb, &temp.path().join("Doc")),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(
        summary.error_kind,
        Some(ErrorKind::Tls),
        "{}",
        brief(&summary)
    );
    assert_eq!(
        summary.message.as_deref(),
        Some("The server doesn't use SSL on this port.")
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// Greets, accepts the user name, then answers the password with `reply`.
async fn login_replying(reply: &'static [u8], accepted: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let _ = wr.write_all(b"200 hi\r\n").await;
                let mut reader = BufReader::new(rd);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line.to_ascii_uppercase().starts_with("AUTHINFO USER") {
                        let _ = wr.write_all(b"381 more\r\n").await;
                    } else {
                        let _ = wr.write_all(reply).await;
                    }
                }
            });
        }
    });
    port
}

/// "502/482 too many connections" in reply to the password is the provider
/// turning one connection too many away, not a wrong password: `Connect`,
/// asked again like a refusing greeting. "481" is still a wrong password.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn too_many_connections_at_login_is_not_a_wrong_password() {
    let temp = tempfile::tempdir().unwrap();
    let (_, ids, sizes, _) = make_file_articles("doc.bin", "d", 2, 1_000);
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let cases = [
        (&b"502 Too many connections\r\n"[..], ErrorKind::Connect, 3),
        (
            &b"482 Too many connections for your user\r\n"[..],
            ErrorKind::Connect,
            3,
        ),
        (&b"481 Authentication failed\r\n"[..], ErrorKind::Auth, 1),
    ];
    for (n, (reply, kind, tries)) in cases.into_iter().enumerate() {
        let label = String::from_utf8_lossy(reply).trim().to_string();
        let accepted = Arc::new(AtomicUsize::new(0));
        let port = login_replying(reply, accepted.clone()).await;
        let usenet = make_config("127.0.0.1", port, ".".into()).usenet;
        let err = Engine::test_connection(&usenet).await.unwrap_err();
        assert_eq!(err.kind(), kind, "{label}: {err}");

        let accepted = Arc::new(AtomicUsize::new(0));
        let port = login_replying(reply, accepted.clone()).await;
        let mut cfg = config(port, temp.path(), 2);
        cfg.usenet.retry_attempts = 2;
        cfg.usenet.retry_delay = 100;
        let engine = Engine::new(cfg).unwrap();
        let summary = wait(
            &engine.start(
                request(&nzb, &temp.path().join(format!("Doc{n}"))),
                Arc::new(Recorder::default()),
            ),
            30,
        )
        .await;
        assert_eq!(
            summary.error_kind,
            Some(kind),
            "{label}: {}",
            brief(&summary)
        );
        assert_eq!(accepted.load(Ordering::SeqCst), tries, "{label}");
        if kind == ErrorKind::Connect {
            let message = summary.message.unwrap_or_default();
            assert!(!message.contains("password"), "{label}: {message}");
        }
    }
}
