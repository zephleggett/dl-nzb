//! A flaky server, one that resets the connection on a random share of
//! `BODY` requests, against articles that always reset it ("poison"). An
//! article that lost the connection three times on its own used to be given
//! up as missing either way, so a flaky server failed jobs with "1-3% of
//! articles are missing" (not resumable). Poison now needs the losses to
//! concentrate on a few articles that never arrive.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{Engine, ErrorKind, Outcome};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

// --- A server that resets at random, the same way every run ------------------------

#[derive(Default)]
struct Srv {
    bodies: Mutex<HashMap<String, Vec<u8>>>,
    /// Always reset these.
    poison: Mutex<Vec<String>>,
    /// Reset this share (%) of the other `BODY` requests.
    flaky_pct: AtomicUsize,
    seed: u64,
    /// `BODY` requests per article so far.
    asked: Mutex<HashMap<String, u64>>,
}

impl Srv {
    fn new(seed: u64, pct: usize) -> Self {
        let srv = Self {
            seed,
            ..Self::default()
        };
        srv.flaky_pct.store(pct, Ordering::SeqCst);
        srv
    }

    fn serve(&self, id: &str, body: Vec<u8>) {
        self.bodies.lock().unwrap().insert(id.into(), body);
    }

    fn most_asked(&self) -> u64 {
        self.asked
            .lock()
            .unwrap()
            .values()
            .copied()
            .max()
            .unwrap_or(0)
    }

    /// Whether to reset the `n`th request for `id`: decided by the seed,
    /// the article and `n` alone, so a run doesn't depend on which
    /// connection asks first.
    fn resets(&self, id: &str, n: u64) -> bool {
        if self.poison.lock().unwrap().iter().any(|p| p == id) {
            return true;
        }
        let mut x = self.seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for b in id.bytes() {
            x = (x ^ b as u64).wrapping_mul(0x0100_0000_01B3);
        }
        x ^= x >> 33;
        x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        x ^= x >> 33;
        ((x % 100) as usize) < self.flaky_pct.load(Ordering::SeqCst)
    }
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
        } else if upper.starts_with("BODY") {
            let id = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            let n = {
                let mut asked = srv.asked.lock().unwrap();
                let n = asked.entry(id.clone()).or_default();
                *n += 1;
                *n
            };
            let body = srv.bodies.lock().unwrap().get(&id).cloned();
            match body {
                Some(_) if srv.resets(&id, n) => {
                    let _ = wr
                        .write_all(b"222 0 body\r\n=ybegin part=1 line=128 size=1000 name=x\r\n")
                        .await;
                    // Closed (FIN) before the reset, as in
                    // review_regressions_round3: macOS can ignore a reset
                    // right behind unacknowledged data (a rate-limited
                    // "challenge ACK"), and the client would wait out its
                    // read timeout instead.
                    let _ = wr.shutdown().await;
                    let sock = reader.into_inner().reunite(wr).unwrap();
                    socket2::SockRef::from(&sock)
                        .set_linger(Some(Duration::ZERO))
                        .unwrap();
                    drop(sock);
                    return;
                }
                Some(body) => {
                    let _ = wr.write_all(b"222 0 body\r\n").await;
                    let _ = wr.write_all(&body).await;
                    let _ = wr.write_all(b".\r\n").await;
                }
                None => {
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
            tokio::spawn(session(sock, srv.clone()));
        }
    });
    port
}

async fn run(srv: Arc<Srv>, temp: &Path, nzb: &Path, par2: bool) -> dl_nzb::JobSummary {
    let port = serve(srv).await;
    let mut cfg = config(port, temp, 2);
    cfg.usenet.retry_attempts = 3;
    cfg.post_processing.auto_par2_repair = par2;
    let engine = Engine::new(cfg).unwrap();
    wait(
        &engine.start(request(nzb, &temp.join("X")), Arc::new(Recorder::default())),
        60,
    )
    .await
}

/// 200 articles, no PAR2, on a server that resets 10%, 30% or 50% of
/// `BODY` requests. None is missing, so the job never says so: it completes,
/// or fails resumably as a connection problem and a resume against a
/// healthy server finishes it. At 30% and 50% the job used to fail as
/// "N% of articles are missing" (not resumable).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flaky_server_never_makes_articles_missing() {
    for (pct, seed) in [(10usize, 1u64), (10, 2), (30, 3), (30, 4), (50, 5), (50, 6)] {
        let (articles, ids, _, full) = make_file_articles("x.bin", "x", 200, 1_000);
        let srv = Arc::new(Srv::new(seed, pct));
        for a in articles {
            srv.serve(&a.message_id, a.body);
        }
        let temp = tempfile::tempdir().unwrap();
        let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &[1064; 200])]);
        let started = Instant::now();
        let first = run(srv.clone(), temp.path(), &nzb, false).await;
        let took = started.elapsed();
        let case = format!("{pct}% seed {seed}: {} in {took:?}", brief(&first));
        assert!(took < Duration::from_secs(15), "{case}");
        if first.outcome != Outcome::Completed {
            assert_eq!(first.error_kind, Some(ErrorKind::Connect), "{case}");
            assert!(first.resumable, "{case}");
            srv.flaky_pct.store(0, Ordering::SeqCst);
            let second = run(srv.clone(), temp.path(), &nzb, false).await;
            assert_eq!(second.outcome, Outcome::Completed, "{}", brief(&second));
        }
        assert_eq!(
            first.articles_failed == 0,
            first.outcome == Outcome::Completed
        );
        let got = std::fs::read(temp.path().join("X").join("x.bin")).unwrap();
        assert!(got == full, "{case}");
        // The case that used to fail: articles reset at least 3 times.
        if pct >= 30 {
            assert!(srv.most_asked() >= 4, "{case}: {}", srv.most_asked());
        }
    }
}

/// On a flaky server, an article that always resets the connection is no
/// more to blame than the rest: after many losses on its own it is given up
/// as lost to the connection (resumable), not missing. Once the server is
/// healthy, a resume finishes the job.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poison_article_on_a_flaky_server_is_a_lost_connection() {
    let (articles, ids, _, full) = make_file_articles("x.bin", "x", 200, 1_000);
    let srv = Arc::new(Srv::new(7, 30));
    for a in articles {
        srv.serve(&a.message_id, a.body);
    }
    srv.poison.lock().unwrap().push(ids[150].clone());
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &[1064; 200])]);
    let first = run(srv.clone(), temp.path(), &nzb, false).await;
    assert_eq!(first.outcome, Outcome::Failed, "{}", brief(&first));
    assert_eq!(
        first.error_kind,
        Some(ErrorKind::Connect),
        "{}",
        brief(&first)
    );
    assert!(first.resumable, "{}", brief(&first));
    assert_eq!(first.articles_failed, 1, "{}", brief(&first));
    assert!(srv.most_asked() >= 12, "{}", srv.most_asked());
    srv.poison.lock().unwrap().clear();
    srv.flaky_pct.store(0, Ordering::SeqCst);
    let second = run(srv, temp.path(), &nzb, false).await;
    assert_eq!(second.outcome, Outcome::Completed, "{}", brief(&second));
    let got = std::fs::read(temp.path().join("X").join("x.bin")).unwrap();
    assert!(got == full);
}

/// Poison articles in a release PAR2 can repair: one, two at the tail, and
/// three side by side (more than `max(2, 1%)` of the job's articles lose
/// the connection on their own, but none of them ever arrives, so they are
/// poison, not a flaky server). Each is given up as missing and repaired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn poison_articles_are_still_left_to_par2() {
    let source = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..60_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(source.path().join("B.bin"), &big).unwrap();
    let par2 = par2_rs::Par2Creator::new(vec![source.path().join("B.bin")])
        .unwrap()
        .with_block_size(2048)
        .unwrap()
        .with_redundancy(30.0)
        .unwrap()
        .create()
        .unwrap();
    for poison in [vec![12usize], vec![28, 29], vec![5, 6, 7]] {
        let srv = Arc::new(Srv::new(0, 0));
        let mut listed: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
        let mut b_ids = Vec::new();
        for k in 0..30 {
            let id = format!("b{k}@t");
            let begin = (k * 2000 + 1) as u64;
            let plain = &big[k * 2000..(k + 1) * 2000];
            srv.serve(
                &id,
                build_part("B.bin", k as u32 + 1, 30, begin, begin + 1999, plain),
            );
            b_ids.push(id);
        }
        *srv.poison.lock().unwrap() = poison.iter().map(|k| b_ids[*k].clone()).collect();
        listed.push(("B.bin".into(), b_ids, vec![2_064; 30]));
        for (i, p) in par2.iter().enumerate() {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(p).unwrap();
            let id = format!("p{i}@t");
            srv.serve(&id, single(&bytes, &name));
            listed.push((name, vec![id], vec![bytes.len() as u64 + 64]));
        }
        let temp = tempfile::tempdir().unwrap();
        let refs: Vec<(&str, &[String], &[u64])> = listed
            .iter()
            .map(|(n, i, s)| (n.as_str(), i.as_slice(), s.as_slice()))
            .collect();
        let nzb = write_nzb(temp.path(), "job", &refs);
        let summary = run(srv.clone(), temp.path(), &nzb, true).await;
        let case = format!("poison {poison:?}: {}", brief(&summary));
        assert_eq!(summary.outcome, Outcome::Completed, "{case}");
        assert!(summary.par2.repaired, "{case}");
        assert_eq!(
            std::fs::read(temp.path().join("X").join("B.bin")).unwrap(),
            big
        );
    }
}

/// Small jobs without PAR2 (an ebook, a single file): a random reset can hit
/// one article three times before the server looks flaky, which used to end
/// as "N% of articles are missing", not resumable. Without recovery data the
/// losses stay the server's: the job completes, or fails resumably and a
/// resume against a healthy server finishes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_small_job_without_par2_is_never_missing_on_a_flaky_server() {
    for n in [8usize, 20] {
        for pct in [20usize, 30, 50] {
            for seed in 1u64..=10 {
                let (articles, ids, _, full) = make_file_articles("s.bin", "s", n, 1_000);
                let srv = Arc::new(Srv::new(seed, pct));
                for a in articles {
                    srv.serve(&a.message_id, a.body);
                }
                let temp = tempfile::tempdir().unwrap();
                let sizes = vec![1064; n];
                let nzb = write_nzb(temp.path(), "s", &[("s.bin", &ids, &sizes)]);
                let first = run(srv.clone(), temp.path(), &nzb, false).await;
                let case = format!("{n} articles, {pct}% seed {seed}: {}", brief(&first));
                if first.outcome != Outcome::Completed {
                    assert_eq!(first.error_kind, Some(ErrorKind::Connect), "{case}");
                    assert!(first.resumable, "{case}");
                    srv.flaky_pct.store(0, Ordering::SeqCst);
                    let second = run(srv.clone(), temp.path(), &nzb, false).await;
                    assert_eq!(
                        second.outcome,
                        Outcome::Completed,
                        "{case} → {}",
                        brief(&second)
                    );
                }
                let got = std::fs::read(temp.path().join("X").join("s.bin")).unwrap();
                assert!(got == full, "{case}");
            }
        }
    }
}
