//! Encrypted RAR archives end to end: passwords from the request, the NZB's
//! `<meta type="password">` and a `{{password}}` file name; wrong and missing
//! passwords finishing as `NeedsPassword` with the download kept; `reprocess`
//! with the right password; `delete_rar_after_extract` only after success; and
//! a prompt stop during extraction.
//!
//! Fixtures are the `unrar` crate's encrypted test archives (MIT/Apache-2.0),
//! the only encrypted RARs we can make or obtain legitimately here (no `rar`
//! binary): `crypted.rar` (RAR 2.9, member data encrypted, password `unrar`;
//! no password check value, so a wrong password shows up as a CRC error) and
//! `comment-hpw-password.rar` (RAR5, headers encrypted, password `password`;
//! a wrong password is refused by its check value). Both hold one member,
//! `.gitignore`. Neither is multi-volume.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, JobEvent, JobHandle, JobObserver, JobPhase, JobRequest, JobSummary, NullObserver,
    Outcome, Preflight,
};

/// The one member of both encrypted fixtures, `.gitignore`.
const MEMBER: &str = ".gitignore";
const MEMBER_DATA: &[u8] = b"target\nCargo.lock\n";

/// The passwords of `CRYPTED_RAR` and `HEADER_ENCRYPTED_RAR`.
const DATA_PASSWORD: &str = "unrar";
const HEADER_PASSWORD: &str = "password";

const NEEDS_PASSWORD: &str = "This archive needs a password.";
const PASSWORD_REFUSED: &str = "The password didn't work.";

/// A mock server posting one archive as `Secret.rar`, and a scratch folder.
struct Release {
    temp: tempfile::TempDir,
    port: u16,
    ids: Vec<String>,
    sizes: Vec<u64>,
}

impl Release {
    async fn serve(archive: &[u8]) -> Self {
        let body = build_part("Secret.rar", 1, 1, 1, archive.len() as u64, archive);
        let state = Arc::new(MockServerState {
            articles: vec![MockArticle {
                message_id: "secret1@t".into(),
                body,
            }],
            ..Default::default()
        });
        let port = spawn_server(state).await;
        Self {
            temp: tempfile::tempdir().unwrap(),
            port,
            ids: vec!["secret1@t".into()],
            sizes: vec![archive.len() as u64 + 64],
        }
    }

    /// Write `<file_stem>.nzb` with `head` (inner `<meta>` elements).
    fn nzb(&self, file_stem: &str, head: &str) -> PathBuf {
        let path = self.temp.path().join(format!("{file_stem}.nzb"));
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head>{head}</head>{}</nzb>"#,
            nzb_file_element("Secret.rar", &self.ids, &self.sizes)
        );
        std::fs::write(&path, xml).unwrap();
        path
    }

    fn folder(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    fn engine(&self, delete_rar_after_extract: bool) -> Engine {
        engine(self.port, self.temp.path(), delete_rar_after_extract)
    }
}

fn engine(port: u16, dir: &Path, delete_rar_after_extract: bool) -> Engine {
    let mut config = make_config("127.0.0.1", port, dir.to_path_buf());
    config.post_processing.auto_extract_rar = true;
    config.post_processing.delete_rar_after_extract = delete_rar_after_extract;
    Engine::new(config).unwrap()
}

fn request(nzb: &Path, out: &Path, passwords: &[&str]) -> JobRequest {
    JobRequest {
        passwords: passwords.iter().map(|p| p.to_string()).collect(),
        preflight: Preflight::Never,
        ..JobRequest::new(nzb, out)
    }
}

async fn run(engine: &Engine, request: JobRequest) -> JobSummary {
    wait(&engine.start(request, Arc::new(NullObserver)), 30).await
}

async fn reprocess(engine: &Engine, dir: &Path, passwords: &[&str]) -> JobSummary {
    let passwords = passwords.iter().map(|p| p.to_string()).collect();
    wait(
        &engine.reprocess(dir.to_path_buf(), passwords, Arc::new(NullObserver)),
        30,
    )
    .await
}

/// The archive extracted into `dir`, with nothing left behind.
fn assert_extracted(summary: &JobSummary, dir: &Path) {
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.message, None);
    assert_eq!(summary.archives_extracted, 1);
    assert_eq!(std::fs::read(dir.join(MEMBER)).unwrap(), MEMBER_DATA);
    assert_no_staging(dir);
}

/// Finished `NeedsPassword` with `message`, the download intact and nothing
/// extracted (no file under the member's name, no staging folder).
fn assert_needs_password(summary: &JobSummary, dir: &Path, message: &str) {
    assert_eq!(summary.outcome, Outcome::NeedsPassword);
    assert_eq!(summary.message.as_deref(), Some(message));
    assert_eq!(summary.archives_extracted, 0);
    assert!(!summary.resumable);
    assert!(dir.join("Secret.rar").is_file(), "the download is kept");
    let names: Vec<&str> = summary.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Secret.rar"]);
    assert!(!dir.join(MEMBER).exists());
    assert_no_staging(dir);
}

fn assert_no_staging(dir: &Path) {
    let leftovers: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".dl-nzb-unpack"))
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_requests_password_extracts_both_kinds_of_encryption() {
    for (archive, password) in [
        (CRYPTED_RAR, DATA_PASSWORD),
        (HEADER_ENCRYPTED_RAR, HEADER_PASSWORD),
    ] {
        let release = Release::serve(archive).await;
        let nzb = release.nzb("Secret", "");
        let out = release.folder("Secret");
        // The newest password is wrong; the next one works.
        let summary = run(
            &release.engine(false),
            request(&nzb, &out, &["newest-but-wrong", password]),
        )
        .await;
        assert_extracted(&summary, &out);
        assert!(
            out.join("Secret.rar").is_file(),
            "kept without delete_rar_after_extract"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_nzbs_meta_password_extracts_it() {
    let release = Release::serve(HEADER_ENCRYPTED_RAR).await;
    let nzb = release.nzb(
        "Secret",
        &format!(r#"<meta type="password">{HEADER_PASSWORD}</meta>"#),
    );
    let out = release.folder("Secret");
    // A wrong password from the user is tried first, then the NZB's.
    let summary = run(&release.engine(false), request(&nzb, &out, &["typo"])).await;
    assert_extracted(&summary, &out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_password_in_the_nzb_file_name_is_used_and_kept_out_of_the_title() {
    let release = Release::serve(CRYPTED_RAR).await;
    let nzb = release.nzb(&format!("Secret Release{{{{{DATA_PASSWORD}}}}}"), "");
    assert!(nzb.to_string_lossy().contains("{{unrar}}"));

    let info = Engine::inspect(&nzb).unwrap();
    assert_eq!(info.title, "Secret Release");
    assert_eq!(info.passwords, vec![DATA_PASSWORD.to_string()]);

    // The app names the job folder after the title.
    let out = release.folder(&info.title);
    let summary = run(&release.engine(false), request(&nzb, &out, &[])).await;
    assert_extracted(&summary, &out);
    assert!(!summary.output_dir.to_string_lossy().contains("{{"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_passwords_need_a_password_and_reprocess_finishes_the_job() {
    for (archive, password) in [
        (CRYPTED_RAR, DATA_PASSWORD),
        (HEADER_ENCRYPTED_RAR, HEADER_PASSWORD),
    ] {
        let release = Release::serve(archive).await;
        let nzb = release.nzb("Secret", "");
        let out = release.folder("Secret");
        let engine = release.engine(false);

        let summary = run(&engine, request(&nzb, &out, &["nope", "also-nope"])).await;
        assert_needs_password(&summary, &out, PASSWORD_REFUSED);
        // The download itself is fine; only extraction is waiting.
        assert_eq!(summary.articles_failed, 0);

        // Another wrong one via reprocess: same answer, still intact.
        let summary = reprocess(&engine, &out, &["still-wrong"]).await;
        assert_needs_password(&summary, &out, PASSWORD_REFUSED);

        let summary = reprocess(&engine, &out, &[password]).await;
        assert_extracted(&summary, &out);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_password_or_only_a_wrong_nzb_one_asks_for_a_password() {
    let release = Release::serve(CRYPTED_RAR).await;
    let engine = release.engine(false);

    let nzb = release.nzb("Secret", "");
    let out = release.folder("None");
    let summary = run(&engine, request(&nzb, &out, &[])).await;
    assert_needs_password(&summary, &out, NEEDS_PASSWORD);

    // The NZB's own password failing is not the user's password failing.
    let nzb = release.nzb("Meta", r#"<meta type="password">not-it</meta>"#);
    let out = release.folder("Meta");
    let summary = run(&engine, request(&nzb, &out, &[])).await;
    assert_needs_password(&summary, &out, NEEDS_PASSWORD);

    let release = Release::serve(HEADER_ENCRYPTED_RAR).await;
    let nzb = release.nzb("Secret", "");
    let out = release.folder("Headers");
    let summary = run(&release.engine(false), request(&nzb, &out, &[])).await;
    assert_needs_password(&summary, &out, NEEDS_PASSWORD);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_archive_still_extracts_beside_one_that_needs_a_password() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Mixed");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Plain.rar"), PLAIN_RAR).unwrap();
    std::fs::write(dir.join("Secret.rar"), CRYPTED_RAR).unwrap();
    let engine = engine(9, temp.path(), false);

    let summary = reprocess(&engine, &dir, &[]).await;
    assert_eq!(summary.outcome, Outcome::NeedsPassword, "never Completed");
    assert_eq!(summary.message.as_deref(), Some(NEEDS_PASSWORD));
    assert_eq!(summary.archives_extracted, 1);
    assert!(dir.join("VERSION").is_file());
    assert!(!dir.join(MEMBER).exists());

    let summary = reprocess(&engine, &dir, &[DATA_PASSWORD]).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(dir.join(MEMBER)).unwrap(), MEMBER_DATA);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_archive_waiting_for_its_password_is_not_renamed() {
    // Deobfuscation renames the biggest obfuscated file; done to one volume
    // of a waiting archive it would break the set before `reprocess`.
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Release");
    std::fs::create_dir_all(&dir).unwrap();
    let obfuscated = "a8f9c0d1e2b3f4a5b6c7d8e9.rar";
    std::fs::write(dir.join(obfuscated), CRYPTED_RAR).unwrap();
    let mut config = make_config("127.0.0.1", 9, temp.path().to_path_buf());
    config.post_processing.auto_extract_rar = true;
    config.post_processing.deobfuscate_file_names = true;
    let engine = Engine::new(config).unwrap();

    let summary = reprocess(&engine, &dir, &[]).await;
    assert_eq!(summary.outcome, Outcome::NeedsPassword);
    assert_eq!(summary.files_renamed, 0);
    assert!(dir.join(obfuscated).is_file());

    let summary = reprocess(&engine, &dir, &[DATA_PASSWORD]).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(dir.join(MEMBER)).unwrap(), MEMBER_DATA);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn archives_are_deleted_only_after_a_successful_extraction() {
    let release = Release::serve(CRYPTED_RAR).await;
    let nzb = release.nzb("Secret", "");
    let out = release.folder("Secret");
    let engine = release.engine(true);

    let summary = run(&engine, request(&nzb, &out, &["wrong"])).await;
    assert_needs_password(&summary, &out, PASSWORD_REFUSED);

    let summary = reprocess(&engine, &out, &[DATA_PASSWORD]).await;
    assert_extracted(&summary, &out);
    assert!(!out.join("Secret.rar").exists(), "deleted once extracted");
}

/// Stops its job as soon as extraction begins.
#[derive(Default)]
struct StopWhenExtracting {
    job: OnceLock<JobHandle>,
    stopped_at: Mutex<Option<Instant>>,
}

impl JobObserver for StopWhenExtracting {
    fn on_event(&self, event: JobEvent) {
        if event != JobEvent::Phase(JobPhase::Extracting) {
            return;
        }
        // The handle is set right after `reprocess` returns.
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.job.get().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        *self.stopped_at.lock().unwrap() = Some(Instant::now());
        self.job.get().expect("job handle").stop();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_extraction_is_prompt_and_leaves_nothing_half_done() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Secret");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Secret.rar"), CRYPTED_RAR).unwrap();
    std::fs::write(dir.join("Plain.rar"), PLAIN_RAR).unwrap();
    let engine = engine(9, temp.path(), true);

    let observer = Arc::new(StopWhenExtracting::default());
    let job = engine.reprocess(
        dir.clone(),
        vec![DATA_PASSWORD.to_string()],
        observer.clone(),
    );
    let _ = observer.job.set(job.clone());
    let summary = wait(&job, 30).await;
    let stopped_at = observer
        .stopped_at
        .lock()
        .unwrap()
        .expect("extraction began");
    assert!(
        stopped_at.elapsed() < Duration::from_secs(2),
        "stop took {:?}",
        stopped_at.elapsed()
    );
    assert_eq!(summary.outcome, Outcome::Stopped);
    assert_eq!(summary.archives_extracted, 0);
    assert!(!dir.join(MEMBER).exists());
    assert!(!dir.join("VERSION").exists());
    assert!(dir.join("Secret.rar").is_file() && dir.join("Plain.rar").is_file());
    assert_no_staging(&dir);

    // Nothing was spoiled: running it again finishes the job.
    let summary = reprocess(&engine, &dir, &[DATA_PASSWORD]).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.archives_extracted, 2);
    assert_eq!(std::fs::read(dir.join(MEMBER)).unwrap(), MEMBER_DATA);
    assert!(dir.join("VERSION").is_file());
}

/// Collects `Warning` events.
#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl JobObserver for Warnings {
    fn on_event(&self, event: JobEvent) {
        if let JobEvent::Warning(warning) = event {
            self.0.lock().unwrap().push(warning);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn damage_under_the_right_rar5_password_is_not_a_wrong_password() {
    // Flip a byte in the RAR5 fixture's encrypted file data (bytes 302..334;
    // its headers stay intact). RAR5 checks the password separately, so the
    // CRC error that follows is damage, reported as such, never as "the
    // password didn't work".
    let mut damaged = HEADER_ENCRYPTED_RAR.to_vec();
    damaged[310] ^= 0x55;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Damaged");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Secret.rar"), &damaged).unwrap();
    let engine = engine(9, temp.path(), true);

    let warnings = Arc::new(Warnings::default());
    let job = engine.reprocess(dir.clone(), vec![HEADER_PASSWORD.into()], warnings.clone());
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::CompletedWithIssues);
    assert_eq!(
        (summary.archives_extracted, summary.archives_failed),
        (0, 1)
    );
    assert_eq!(
        *warnings.0.lock().unwrap(),
        vec!["Could not extract Secret.rar: the data is damaged.".to_string()]
    );
    // Nothing half-written under the member's name; the archive is kept even
    // with delete_rar_after_extract.
    assert!(!dir.join(MEMBER).exists());
    assert!(dir.join("Secret.rar").is_file());
    assert_no_staging(&dir);
}
