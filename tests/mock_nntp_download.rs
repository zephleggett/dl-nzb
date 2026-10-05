//! End-to-end test against a mock NNTP server.
//!
//! Spins up a tiny TCP listener that speaks just enough NNTP to satisfy the
//! downloader: greeting, AUTHINFO, GROUP, BODY (pipelined), STAT, NOOP, QUIT.
//! The articles are yEnc-encoded with `=ypart` headers so we exercise the
//! offset-aware decoder. We then run a synthetic NZB through `Downloader` and
//! assert the assembled file matches the original byte-for-byte.

mod common;

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use common::*;
use dl_nzb::download::{Downloader, Nzb};

#[tokio::test]
async fn downloads_complete_file_with_correct_offsets() {
    let port = pick_free_port();
    let temp = tempfile::tempdir().unwrap();
    let download_dir = temp.path().join("out");
    std::fs::create_dir_all(&download_dir).unwrap();

    // 5 segments, each 50KB of distinct pseudo-random bytes (cycle of indices).
    let segment_size = 50_000usize;
    let segment_count = 5usize;
    let mut full_data = Vec::with_capacity(segment_size * segment_count);
    for i in 0..(segment_size * segment_count) {
        full_data.push(((i * 31 + 7) % 256) as u8);
    }

    let mut articles = Vec::new();
    let mut message_ids = Vec::new();
    let mut encoded_sizes = Vec::new();
    for part in 0..segment_count {
        let begin = (part * segment_size) as u64 + 1;
        let end = ((part + 1) * segment_size) as u64;
        let plain = &full_data[part * segment_size..(part + 1) * segment_size];
        let body = build_part(
            "test.bin",
            (part + 1) as u32,
            segment_count as u32,
            begin,
            end,
            plain,
        );
        let id = format!("seg{}@test.local", part + 1);
        // Use a slightly inflated encoded size, simulating real NZBs.
        encoded_sizes.push((segment_size as u64) * 102 / 100);
        message_ids.push(id.clone());
        articles.push(MockArticle {
            message_id: id,
            body,
        });
    }

    let state = Arc::new(MockServerState {
        articles,
        missing_ids: Vec::new(),
        error_412_ids: Vec::new(),
        body_requests: Default::default(),
        ..Default::default()
    });
    let ready = Arc::new(Notify::new());
    let server_state = state.clone();
    let server_ready = ready.clone();
    tokio::spawn(async move { start_mock_server(server_state, port, server_ready).await });
    ready.notified().await;

    let xml = build_synthetic_nzb("test.bin", &message_ids, &encoded_sizes);
    let nzb_path = temp.path().join("test.nzb");
    let mut f = std::fs::File::create(&nzb_path).unwrap();
    f.write_all(xml.as_bytes()).unwrap();
    drop(f);

    let nzb = Nzb::from_file(&nzb_path).unwrap();
    let config = make_config("127.0.0.1", port, download_dir.clone());
    let downloader = Downloader::new(config.clone()).await.unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        downloader.download_nzb(&nzb, config, None),
    )
    .await
    .expect("download timeout")
    .expect("download failed");

    let results = result.files;
    assert_eq!(results.len(), 1, "expected single file");
    let r = &results[0];
    assert_eq!(r.segments_failed, 0);
    assert_eq!(r.segments_downloaded, segment_count);
    assert_eq!(r.size, full_data.len() as u64);

    let mut on_disk = Vec::new();
    let path = download_dir.join("test.bin");
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_to_end(&mut on_disk))
        .unwrap();
    assert_eq!(on_disk, full_data, "file content mismatch");
}

#[tokio::test]
async fn handles_missing_segments_gracefully() {
    let port = pick_free_port();
    let temp = tempfile::tempdir().unwrap();
    let download_dir = temp.path().join("out");
    std::fs::create_dir_all(&download_dir).unwrap();

    let segment_size = 10_000usize;
    let segment_count = 3usize;
    let mut full_data = Vec::with_capacity(segment_size * segment_count);
    for i in 0..(segment_size * segment_count) {
        full_data.push((i & 0xff) as u8);
    }

    let mut articles = Vec::new();
    let mut message_ids = Vec::new();
    let mut encoded_sizes = Vec::new();
    for part in 0..segment_count {
        let begin = (part * segment_size) as u64 + 1;
        let end = ((part + 1) * segment_size) as u64;
        let plain = &full_data[part * segment_size..(part + 1) * segment_size];
        let body = build_part(
            "test.bin",
            (part + 1) as u32,
            segment_count as u32,
            begin,
            end,
            plain,
        );
        let id = format!("seg{}@test.local", part + 1);
        encoded_sizes.push((segment_size as u64) + 100);
        message_ids.push(id.clone());
        articles.push(MockArticle {
            message_id: id,
            body,
        });
    }

    // Mark the middle segment as missing.
    let missing = vec![message_ids[1].clone()];

    let state = Arc::new(MockServerState {
        articles,
        missing_ids: missing,
        error_412_ids: Vec::new(),
        body_requests: Default::default(),
        ..Default::default()
    });
    let ready = Arc::new(Notify::new());
    let server_state = state.clone();
    let server_ready = ready.clone();
    tokio::spawn(async move { start_mock_server(server_state, port, server_ready).await });
    ready.notified().await;

    let xml = build_synthetic_nzb("test.bin", &message_ids, &encoded_sizes);
    let nzb_path = temp.path().join("test.nzb");
    let mut f = std::fs::File::create(&nzb_path).unwrap();
    f.write_all(xml.as_bytes()).unwrap();
    drop(f);

    let nzb = Nzb::from_file(&nzb_path).unwrap();
    let config = make_config("127.0.0.1", port, download_dir.clone());
    let downloader = Downloader::new(config.clone()).await.unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        downloader.download_nzb(&nzb, config, None),
    )
    .await
    .expect("download timeout")
    .expect("download failed");

    let results = result.files;
    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert_eq!(r.segments_downloaded, 2);
    assert_eq!(r.segments_failed, 1);
    // The first and third segment should still be present at their offsets.
    let path = download_dir.join("test.bin");
    let on_disk = std::fs::read(&path).unwrap();
    assert!(on_disk.len() >= 2 * segment_size); // partial but extends past the gap
    assert_eq!(&on_disk[..segment_size], &full_data[..segment_size]);
}

/// A server that always answers 412 ("no newsgroup selected") for an article
/// must NOT livelock: the article is retried a bounded number of times then
/// permanently failed, and its neighbours still succeed. (Regression guard for
/// the uncapped-transient-retry / group_ready-latch hang.)
#[tokio::test]
async fn persistent_412_does_not_hang() {
    let port = pick_free_port();
    let temp = tempfile::tempdir().unwrap();
    let download_dir = temp.path().join("out");
    std::fs::create_dir_all(&download_dir).unwrap();

    let segment_size = 5_000usize;
    let segment_count = 3usize;
    let mut full_data = Vec::with_capacity(segment_size * segment_count);
    for i in 0..(segment_size * segment_count) {
        full_data.push(((i * 11 + 1) % 256) as u8);
    }
    let mut articles = Vec::new();
    let mut message_ids = Vec::new();
    let mut encoded_sizes = Vec::new();
    for part in 0..segment_count {
        let begin = (part * segment_size) as u64 + 1;
        let end = ((part + 1) * segment_size) as u64;
        let plain = &full_data[part * segment_size..(part + 1) * segment_size];
        let body = build_part(
            "test.bin",
            (part + 1) as u32,
            segment_count as u32,
            begin,
            end,
            plain,
        );
        let id = format!("seg{}@t", part + 1);
        encoded_sizes.push((segment_size as u64) + 60);
        message_ids.push(id.clone());
        articles.push(MockArticle {
            message_id: id,
            body,
        });
    }

    let state = Arc::new(MockServerState {
        articles,
        missing_ids: Vec::new(),
        error_412_ids: vec!["seg2@t".to_string()], // middle article always 412s
        body_requests: Default::default(),
        ..Default::default()
    });
    let ready = Arc::new(Notify::new());
    let (ss, sr) = (state.clone(), ready.clone());
    tokio::spawn(async move { start_mock_server(ss, port, sr).await });
    ready.notified().await;

    let xml = build_synthetic_nzb("test.bin", &message_ids, &encoded_sizes);
    let nzb_path = temp.path().join("test.nzb");
    std::fs::write(&nzb_path, xml).unwrap();

    let nzb = Nzb::from_file(&nzb_path).unwrap();
    let config = make_config("127.0.0.1", port, download_dir.clone());
    // retry_attempts=2 -> transient cap = 2*5 = 10.
    let downloader = Downloader::new(config.clone()).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        downloader.download_nzb(&nzb, config, None),
    )
    .await
    .expect("must not hang on persistent 412")
    .expect("download ok");

    let r = &result.files[0];
    assert_eq!(r.segments_failed, 1, "the 412 article permanently fails");
    assert_eq!(r.segments_downloaded, 2, "neighbours succeed");
    let n = state
        .body_requests
        .lock()
        .unwrap()
        .get("seg2@t")
        .copied()
        .unwrap_or(0);
    assert!(n >= 2, "must retry the 412 (got {n})");
    assert!(n <= 12, "must be bounded, not infinite (got {n})");
}

/// PAR2-on-demand: when the data is complete, recovery volumes are NEVER
/// fetched; when a data segment is missing, the recovery volume IS fetched.
#[tokio::test]
async fn par2_recovery_is_deferred_until_needed() {
    async fn run(drop_data: bool) -> (Arc<MockServerState>, dl_nzb::download::DownloadOutcome) {
        let port = pick_free_port();
        let temp = tempfile::tempdir().unwrap();
        let download_dir = temp.path().join("out");
        std::fs::create_dir_all(&download_dir).unwrap();

        let (a_d1, s_d1) = make_single_article("movie.bin", "d1@t", 6000);
        let (a_d2, s_d2) = make_single_article("movie.bin", "d2@t", 6000);
        let (a_idx, s_idx) = make_single_article("movie.par2", "idx@t", 1000);
        let (a_vol, s_vol) = make_single_article("movie.vol00+01.par2", "vol@t", 6000);

        let missing = if drop_data {
            vec!["d2@t".to_string()]
        } else {
            vec![]
        };
        let state = Arc::new(MockServerState {
            articles: vec![a_d1, a_d2, a_idx, a_vol],
            missing_ids: missing,
            error_412_ids: Vec::new(),
            body_requests: Default::default(),
            ..Default::default()
        });
        let ready = Arc::new(Notify::new());
        let (ss, sr) = (state.clone(), ready.clone());
        tokio::spawn(async move { start_mock_server(ss, port, sr).await });
        ready.notified().await;

        let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        xml.push_str(r#"<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">"#);
        xml.push_str(&nzb_file_element(
            "movie.bin",
            &["d1@t".into(), "d2@t".into()],
            &[s_d1, s_d2],
        ));
        xml.push_str(&nzb_file_element("movie.par2", &["idx@t".into()], &[s_idx]));
        xml.push_str(&nzb_file_element(
            "movie.vol00+01.par2",
            &["vol@t".into()],
            &[s_vol],
        ));
        xml.push_str("</nzb>");
        let nzb_path = temp.path().join("movie.nzb");
        std::fs::write(&nzb_path, xml).unwrap();

        let nzb = Nzb::from_file(&nzb_path).unwrap();
        let config = make_config("127.0.0.1", port, download_dir.clone());
        let downloader = Downloader::new(config.clone()).await.unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            downloader.download_nzb_on_demand(&nzb, config, None, false),
        )
        .await
        .expect("no hang")
        .expect("download ok");
        (state, outcome)
    }

    let req =
        |state: &MockServerState, id: &str| state.body_requests.lock().unwrap().get(id).copied();

    // Data complete -> recovery volume never requested.
    let (state, _outcome) = run(false).await;
    assert_eq!(req(&state, "d1@t"), Some(1));
    assert_eq!(req(&state, "d2@t"), Some(1));
    assert_eq!(req(&state, "idx@t"), Some(1), "index downloaded");
    assert_eq!(
        req(&state, "vol@t"),
        None,
        "recovery volume must NOT be fetched when data is intact"
    );

    // Data missing -> recovery volume fetched for repair.
    let (state2, outcome2) = run(true).await;
    assert_eq!(
        req(&state2, "vol@t"),
        Some(1),
        "recovery volume must be fetched when a data segment is missing"
    );
    let bin = outcome2
        .files
        .iter()
        .find(|f| f.filename == "movie.bin")
        .unwrap();
    assert_eq!(bin.segments_failed, 1);
}

/// Flip one hex digit of the `pcrc32=` field in a yEnc body so the decoded data
/// is correct but the per-part CRC check fails — exercising the DecodeFailed
/// path without corrupting the line structure.
fn corrupt_pcrc32(body: &mut [u8]) {
    let needle = b"pcrc32=";
    let pos = body
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("pcrc32 present");
    let hex_pos = pos + needle.len();
    // Flip the first hex digit to a different valid hex char.
    body[hex_pos] = if body[hex_pos] == b'0' { b'1' } else { b'0' };
}

/// A persistent yEnc decode failure (bad pcrc32) must be retried up to
/// `decode_retry_cap` times and then permanently failed — never retried forever,
/// and without poisoning the connection (its neighbors still succeed).
#[tokio::test]
async fn decode_failure_is_retried_then_failed() {
    let port = pick_free_port();
    let temp = tempfile::tempdir().unwrap();
    let download_dir = temp.path().join("out");
    std::fs::create_dir_all(&download_dir).unwrap();

    let segment_size = 8_000usize;
    let segment_count = 3usize;
    let mut full_data = Vec::with_capacity(segment_size * segment_count);
    for i in 0..(segment_size * segment_count) {
        full_data.push(((i * 7 + 3) % 256) as u8);
    }

    let mut articles = Vec::new();
    let mut message_ids = Vec::new();
    let mut encoded_sizes = Vec::new();
    for part in 0..segment_count {
        let begin = (part * segment_size) as u64 + 1;
        let end = ((part + 1) * segment_size) as u64;
        let plain = &full_data[part * segment_size..(part + 1) * segment_size];
        let mut body = build_part(
            "test.bin",
            (part + 1) as u32,
            segment_count as u32,
            begin,
            end,
            plain,
        );
        // Corrupt the CRC of the middle segment only.
        if part == 1 {
            corrupt_pcrc32(&mut body);
        }
        let id = format!("seg{}@test.local", part + 1);
        encoded_sizes.push((segment_size as u64) + 80);
        message_ids.push(id.clone());
        articles.push(MockArticle {
            message_id: id,
            body,
        });
    }

    let state = Arc::new(MockServerState {
        articles,
        missing_ids: Vec::new(),
        error_412_ids: Vec::new(),
        body_requests: Default::default(),
        ..Default::default()
    });
    let ready = Arc::new(Notify::new());
    let server_state = state.clone();
    let server_ready = ready.clone();
    tokio::spawn(async move { start_mock_server(server_state, port, server_ready).await });
    ready.notified().await;

    let xml = build_synthetic_nzb("test.bin", &message_ids, &encoded_sizes);
    let nzb_path = temp.path().join("test.nzb");
    let mut f = std::fs::File::create(&nzb_path).unwrap();
    f.write_all(xml.as_bytes()).unwrap();
    drop(f);

    let nzb = Nzb::from_file(&nzb_path).unwrap();
    let mut config = make_config("127.0.0.1", port, download_dir.clone());
    config.tuning.decode_retry_cap = 2; // 2 attempts total for a decode failure

    let downloader = Downloader::new(config.clone()).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        downloader.download_nzb(&nzb, config, None),
    )
    .await
    .expect("download should not hang")
    .expect("download failed");

    let r = &result.files[0];
    assert_eq!(r.segments_downloaded, 2, "two good segments written");
    assert_eq!(
        r.segments_failed, 1,
        "the corrupt segment permanently failed"
    );

    // The corrupt segment was requested exactly `decode_retry_cap` (2) times —
    // proving it was retried once and then given up (no infinite loop).
    let counts = state.body_requests.lock().unwrap();
    assert_eq!(
        counts.get("seg2@test.local").copied(),
        Some(2),
        "corrupt segment retried up to the decode cap then failed"
    );
    // The good neighbors were each fetched exactly once (connection not poisoned
    // by a decode failure).
    assert_eq!(counts.get("seg1@test.local").copied(), Some(1));
    assert_eq!(counts.get("seg3@test.local").copied(), Some(1));

    // Good segments landed at their correct offsets.
    let on_disk = std::fs::read(download_dir.join("test.bin")).unwrap();
    assert_eq!(&on_disk[..segment_size], &full_data[..segment_size]);
}
