//! A small safe layer over the unRAR DLL API (`unrar_sys`), used for listing,
//! testing and extracting RAR archives.
//!
//! The `unrar` crate's wrappers install a callback of their own that can
//! neither interrupt a member nor say how far it got. This layer owns the
//! callback instead:
//! - it hands unRAR the password as a wide string when unRAR asks for it
//!   (also at open time, which archives with encrypted headers need), so
//!   non-ASCII passwords arrive intact on every platform;
//! - it refuses to wait for a missing volume (unRAR reports `EOpen`);
//! - it counts unpacked bytes for progress;
//! - it stops unRAR as soon as the job's cancel flag is set, on the next chunk
//!   unpacked or the next volume opened (reported as [`RarError::Cancelled`]).
//!   A member skipped in a solid archive is tested instead, because unRAR
//!   decompresses it either way but only calls back while testing.
//!
//! unRAR keeps process-wide state (its error handler, which every open resets
//! and every error writes), so only one archive may be in use at a time in
//! the whole process: opening one takes an [`UnrarLock`], which a job waits
//! for (giving up as soon as it is stopped) while another job's archive is
//! open.
//!
//! An archive handle is used from one thread at a time (it is not `Send`):
//! the extractor opens, uses and drops it inside one blocking task.

use std::ffi::{c_int, c_uint};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use unrar::error::Code;
use unrar_sys as native;
use widestring::{WideCString, WideChar};

/// The longest password, in wide characters, that unRAR's password buffer
/// (`MAXPASSWORD`, 512 with the NUL) takes whole. A longer one would be cut
/// short, so it is refused. (Of what it takes, RAR derives its keys from the
/// first 127 characters, both when it creates an archive and here.)
pub(crate) const MAX_PASSWORD_CHARS: usize = 511;

/// How often a job waiting for unRAR checks whether it was stopped.
const LOCK_POLL: Duration = Duration::from_millis(10);

/// Room for a link's target name (unRAR's `MAXPATHSIZE`).
const REDIR_NAME_MAX: usize = 0x10000;

/// unRAR's `FILE_SYSTEM_REDIRECT` values used here.
const FSREDIR_NONE: c_uint = 0;
const FSREDIR_FILECOPY: c_uint = 5;

static UNRAR: Mutex<()> = Mutex::new(());

/// Exclusive use of unRAR by this thread. Every [`RarArchive`] borrows one,
/// so no archive outlives it.
pub(crate) struct UnrarLock {
    _guard: MutexGuard<'static, ()>,
}

/// Take unRAR for this thread, waiting while another job uses it. `None`
/// once `cancel` is set: a stopped job does not wait for another job's
/// extraction to end.
pub(crate) fn lock_unrar(cancel: &AtomicBool) -> Option<UnrarLock> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match UNRAR.try_lock() {
            Ok(guard) => return Some(UnrarLock { _guard: guard }),
            // A panic while unRAR was in use closed its archive on the way
            // out (`RarArchive`'s drop), so there is nothing to recover.
            Err(TryLockError::Poisoned(poisoned)) => {
                return Some(UnrarLock {
                    _guard: poisoned.into_inner(),
                })
            }
            Err(TryLockError::WouldBlock) => std::thread::sleep(LOCK_POLL),
        }
    }
}

/// How an archive is opened: listing walks the headers (one entry per file,
/// however many volumes it spans); extraction can also test and unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    List,
    Extract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RarError {
    /// The job's cancel flag stopped unRAR in the middle of a member.
    Cancelled,
    /// unRAR's own error code.
    Code(Code),
}

/// What a member is, beyond a plain file or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Redirect {
    None,
    /// A symbolic or hard link, or a junction.
    Link,
    /// A copy of another member, named as in the archive (RAR5 can store
    /// identical files once).
    Copy(PathBuf),
}

/// One archive header.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) name: PathBuf,
    pub(crate) unpacked_size: u64,
    pub(crate) encrypted: bool,
    pub(crate) directory: bool,
    /// A continuation of a file that started in an earlier volume (seen in
    /// extract mode after skipping a split file).
    pub(crate) split_before: bool,
    /// Version needed to unpack: 50 or more for RAR5, whose encrypted members
    /// carry a password check value (a wrong password is reported as such,
    /// never as a CRC error).
    pub(crate) unpack_version: u32,
    pub(crate) redirect: Redirect,
}

impl Entry {
    pub(crate) fn is_rar5(&self) -> bool {
        self.unpack_version >= 50
    }
}

/// Called with the size of every unpacked chunk.
pub(crate) type DataSink = Box<dyn FnMut(u64)>;

/// A password, wiped from memory when dropped. Deliberately not `Debug`:
/// passwords are never logged.
pub(crate) struct Password(String);

impl Password {
    pub(crate) fn new(password: &str) -> Self {
        Self(password.to_owned())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Its length in the wide characters unRAR takes.
    pub(crate) fn wide_len(&self) -> usize {
        if std::mem::size_of::<WideChar>() == 2 {
            self.0.encode_utf16().count()
        } else {
            self.0.chars().count()
        }
    }

    fn wipe(&mut self) {
        // SAFETY: only zero bytes are written, which keep the string UTF-8.
        wipe(unsafe { self.0.as_mut_vec() });
    }
}

impl Clone for Password {
    fn clone(&self) -> Self {
        Self::new(&self.0)
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        self.wipe();
    }
}

/// Overwrite `buf` with zeros, in a way the compiler can't leave out.
fn wipe<T: Copy + Default>(buf: &mut [T]) {
    for slot in buf.iter_mut() {
        // SAFETY: `slot` is a valid, aligned and exclusive reference.
        unsafe { std::ptr::write_volatile(slot, T::default()) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// What the callback needs, owned by the archive and handed to unRAR as the
/// callback's user data (so its address must not move: it is boxed).
struct CallbackState {
    /// The password as wide characters, without a NUL; wiped on drop.
    password: Option<Vec<WideChar>>,
    cancel: Arc<AtomicBool>,
    /// Set when the callback refused to continue because of `cancel`.
    cancelled: bool,
    on_data: Option<DataSink>,
}

impl Drop for CallbackState {
    fn drop(&mut self) {
        if let Some(password) = self.password.as_mut() {
            wipe(password);
        }
    }
}

/// An open archive. Closed on drop.
pub(crate) struct RarArchive<'lock> {
    handle: NonNull<native::Handle>,
    state: NonNull<CallbackState>,
    flags: u32,
    mode: Mode,
    /// Where unRAR writes a link's target name, reused for every header
    /// (allocated on the first).
    redir_name: Vec<native::WCHAR>,
    _lock: PhantomData<&'lock UnrarLock>,
}

impl<'lock> RarArchive<'lock> {
    /// Open `path` (the first volume of a set), holding `lock` for as long as
    /// the archive is open. `password` is given to unRAR whenever it asks for
    /// one; `cancel` stops unRAR at its next chunk or volume.
    pub(crate) fn open(
        path: &Path,
        mode: Mode,
        password: Option<&str>,
        cancel: Arc<AtomicBool>,
        _lock: &'lock UnrarLock,
    ) -> Result<Self, RarError> {
        let password = match password {
            Some(p) => {
                // A password with a NUL can't be typed into RAR, and one
                // longer than unRAR takes would be cut short: neither can be
                // the right one.
                let mut wide = WideCString::from_str(p)
                    .map_err(|_| RarError::Code(Code::BadPassword))?
                    .into_vec();
                if wide.len() > MAX_PASSWORD_CHARS {
                    wipe(&mut wide);
                    return Err(RarError::Code(Code::BadPassword));
                }
                Some(wide)
            }
            None => None,
        };
        let name = os::rar_string(path).ok_or(RarError::Code(Code::EOpen))?;
        let state = NonNull::from(Box::leak(Box::new(CallbackState {
            password,
            cancel,
            cancelled: false,
            on_data: None,
        })));

        let open_mode = match mode {
            Mode::List => native::RAR_OM_LIST,
            Mode::Extract => native::RAR_OM_EXTRACT,
        };
        let mut data = native::OpenArchiveDataEx::new(name.as_ptr().cast(), open_mode);
        data.callback = Some(callback);
        data.user_data = state.as_ptr() as native::LPARAM;
        // SAFETY: `data` and the name it points to outlive the call. The
        // callback's user data stays valid until the handle is closed: it is
        // freed only after `RARCloseArchive` (here on failure, or in `Drop`).
        // The caller's `UnrarLock` keeps every other thread out of unRAR.
        let handle = unsafe { native::RAROpenArchiveEx(std::ptr::addr_of_mut!(data)) };
        let result = data.open_result as c_int;
        match NonNull::new(handle as *mut native::Handle) {
            Some(handle) if result == native::ERAR_SUCCESS => Ok(Self {
                handle,
                state,
                flags: data.flags,
                mode,
                redir_name: Vec::new(),
                _lock: PhantomData,
            }),
            handle => {
                // SAFETY: a handle returned with an error must still be
                // closed; `state` is not used by unRAR after that.
                unsafe {
                    if let Some(handle) = handle {
                        native::RARCloseArchive(handle.as_ptr());
                    }
                    drop(Box::from_raw(state.as_ptr()));
                }
                Err(RarError::Code(code(result)))
            }
        }
    }

    /// The archive's file names are encrypted too (`rar -hp`).
    pub(crate) fn headers_encrypted(&self) -> bool {
        self.flags & native::ROADF_ENCHEADERS != 0
    }

    /// One compressed stream for all members: a member can only be unpacked
    /// after the ones before it.
    pub(crate) fn is_solid(&self) -> bool {
        self.flags & native::ROADF_SOLID != 0
    }

    /// The next header, or `None` at the end of the set. Must be followed by
    /// [`skip`](Self::skip), [`test`](Self::test) or
    /// [`extract_to`](Self::extract_to).
    pub(crate) fn read_header(&mut self) -> Result<Option<Entry>, RarError> {
        // SAFETY: every field is an integer, an array of them or a raw
        // pointer, all valid as zeros (null pointers).
        let mut header: Box<HeaderData> = Box::new(unsafe { std::mem::zeroed() });
        if self.redir_name.is_empty() {
            self.redir_name = vec![0; REDIR_NAME_MAX];
        }
        // Nothing of the last member's link name may pass for this one's.
        self.redir_name[0] = 0;
        header.redir_name = self.redir_name.as_mut_ptr();
        header.redir_name_size = self.redir_name.len() as c_uint;
        self.state().cancelled = false;
        // SAFETY: the handle is open and `header` is a writable, zeroed
        // `RARHeaderDataEx` laid out as unRAR declares it (with slack in case
        // this unRAR's struct is larger), whose link-name pointer and size
        // describe `redir_name`, alive until after the call. unRAR only sees
        // the pointer's bytes, so the binding's pointer type doesn't matter.
        let result = unsafe {
            native::RARReadHeaderEx(
                self.handle.as_ptr(),
                std::ptr::addr_of_mut!(*header).cast::<native::HeaderDataEx>(),
            )
        };
        match result {
            native::ERAR_SUCCESS => Ok(Some(entry(&header, &self.redir_name))),
            native::ERAR_END_ARCHIVE => Ok(None),
            other => Err(self.error(other)),
        }
    }

    /// Move past the current member without unpacking it. In a solid archive
    /// unRAR still has to decompress it (into memory) and, asked to skip,
    /// never calls back meanwhile, so a stop would go unseen: there it is
    /// tested instead, the same work (checksum included) but with the data
    /// callback that sees a stop.
    pub(crate) fn skip(&mut self) -> Result<(), RarError> {
        let operation = if self.mode == Mode::Extract && self.is_solid() {
            native::RAR_TEST
        } else {
            native::RAR_SKIP
        };
        self.process(operation, None, None)
    }

    /// Unpack the current member and check its checksum, writing nothing.
    pub(crate) fn test(&mut self, on_data: Option<DataSink>) -> Result<(), RarError> {
        self.process(native::RAR_TEST, None, on_data)
    }

    /// Unpack the current member into the file `dest` (overwriting it).
    pub(crate) fn extract_to(
        &mut self,
        dest: &Path,
        on_data: Option<DataSink>,
    ) -> Result<(), RarError> {
        self.process(native::RAR_EXTRACT, Some(dest), on_data)
    }

    fn process(
        &mut self,
        operation: c_int,
        dest: Option<&Path>,
        on_data: Option<DataSink>,
    ) -> Result<(), RarError> {
        let dest = match dest {
            Some(path) => Some(os::rar_string(path).ok_or(RarError::Code(Code::ECreate))?),
            None => None,
        };
        {
            let state = self.state();
            state.cancelled = false;
            state.on_data = on_data;
        }
        // SAFETY: the handle is open and positioned after a header; `dest`
        // outlives the call.
        let result = unsafe { os::process_file(self.handle.as_ptr(), operation, dest.as_ref()) };
        self.state().on_data = None;
        match result {
            native::ERAR_SUCCESS => Ok(()),
            other => Err(self.error(other)),
        }
    }

    fn error(&mut self, result: c_int) -> RarError {
        if self.state().cancelled {
            RarError::Cancelled
        } else {
            RarError::Code(code(result))
        }
    }

    fn state(&mut self) -> &mut CallbackState {
        // SAFETY: `state` is owned by `self` and unRAR only touches it from
        // inside the calls above, which never overlap with this borrow.
        unsafe { self.state.as_mut() }
    }
}

impl Drop for RarArchive<'_> {
    fn drop(&mut self) {
        // SAFETY: closing the open handle, then freeing the callback state
        // unRAR no longer references.
        unsafe {
            native::RARCloseArchive(self.handle.as_ptr());
            drop(Box::from_raw(self.state.as_ptr()));
        }
    }
}

/// unRAR's `RARHeaderDataEx` as `dll.hpp` declares it: packed, unlike the
/// `unrar_sys` binding's struct, which pads before its first pointer and so
/// misplaces every field from there on (harmless while they are all zero,
/// but the link-name pointer must be where unRAR reads it). Room to spare at
/// the end, in case this unRAR's struct is larger.
#[repr(C, packed)]
#[allow(dead_code)] // written by unRAR, read field by field
struct HeaderData {
    arc_name: [std::ffi::c_char; 1024],
    arc_name_w: [native::WCHAR; 1024],
    file_name: [std::ffi::c_char; 1024],
    file_name_w: [native::WCHAR; 1024],
    flags: c_uint,
    pack_size: c_uint,
    pack_size_high: c_uint,
    unp_size: c_uint,
    unp_size_high: c_uint,
    host_os: c_uint,
    file_crc: c_uint,
    file_time: c_uint,
    unp_ver: c_uint,
    method: c_uint,
    file_attr: c_uint,
    cmt_buf: *mut std::ffi::c_char,
    cmt_buf_size: c_uint,
    cmt_size: c_uint,
    cmt_state: c_uint,
    dict_size: c_uint,
    hash_type: c_uint,
    hash: [std::ffi::c_char; 32],
    redir_type: c_uint,
    redir_name: *mut native::WCHAR,
    redir_name_size: c_uint,
    dir_target: c_uint,
    times: [c_uint; 6],
    arc_name_ex: *mut native::WCHAR,
    arc_name_ex_size: c_uint,
    file_name_ex: *mut native::WCHAR,
    file_name_ex_size: c_uint,
    reserved: [c_uint; 982],
    slack: [u8; 256],
}

// Up to the first pointer the binding's layout is the same (the fields the
// binding was used for so far).
const _: () = assert!(
    std::mem::offset_of!(HeaderData, file_attr)
        == std::mem::offset_of!(native::HeaderDataEx, file_attr)
);

/// A NUL-terminated wide string from a buffer unRAR filled.
fn wide_path(buffer: &[native::WCHAR]) -> PathBuf {
    // SAFETY: the buffer is readable for its whole length (`wchar_t` and
    // `WideChar` have the same size); the name ends at the first NUL or at
    // the end of the buffer.
    let name =
        unsafe { WideCString::from_ptr_truncate(buffer.as_ptr().cast::<WideChar>(), buffer.len()) };
    PathBuf::from(name.to_os_string())
}

fn entry(header: &HeaderData, redir_name: &[native::WCHAR]) -> Entry {
    // Fields of a packed struct are copied out, never borrowed.
    let (flags, file_name, redir_type) = (header.flags, header.file_name_w, header.redir_type);
    let redirect = match redir_type {
        FSREDIR_NONE => Redirect::None,
        FSREDIR_FILECOPY => Redirect::Copy(wide_path(redir_name)),
        _ => Redirect::Link,
    };
    Entry {
        name: wide_path(&file_name),
        unpacked_size: ((header.unp_size_high as u64) << 32) | header.unp_size as u64,
        encrypted: flags & native::RHDF_ENCRYPTED != 0,
        directory: flags & native::RHDF_DIRECTORY != 0,
        split_before: flags & native::RHDF_SPLITBEFORE != 0,
        unpack_version: header.unp_ver,
        redirect,
    }
}

fn code(result: c_int) -> Code {
    Code::from(result).unwrap_or(Code::Unknown)
}

/// The unRAR callback. `user_data` is the archive's [`CallbackState`].
extern "C" fn callback(
    msg: native::UINT,
    user_data: native::LPARAM,
    p1: native::LPARAM,
    p2: native::LPARAM,
) -> c_int {
    if user_data == 0 {
        return -1;
    }
    // SAFETY: `user_data` is the boxed state installed at open, alive until
    // the handle is closed, and only accessed from inside unRAR calls.
    let state = unsafe { &mut *(user_data as *mut CallbackState) };
    // Never unwind into C.
    catch_unwind(AssertUnwindSafe(|| match msg {
        native::UCM_CHANGEVOLUMEW | native::UCM_CHANGEVOLUME => {
            if p2 == native::RAR_VOL_ASK {
                // A missing volume: don't wait for one (it is not coming).
                -1
            } else if state.cancel.load(Ordering::Relaxed) {
                // About to open the next volume: a stop ends it here.
                state.cancelled = true;
                -1
            } else {
                0
            }
        }
        native::UCM_PROCESSDATA => {
            if state.cancel.load(Ordering::Relaxed) {
                state.cancelled = true;
                return -1;
            }
            if let Some(on_data) = state.on_data.as_mut() {
                on_data(p2.max(0) as u64);
            }
            0
        }
        native::UCM_NEEDPASSWORDW => match &state.password {
            // `open` refused a password too long for the buffer.
            Some(password) if p1 != 0 && p2 > password.len() as native::LPARAM => {
                // SAFETY: unRAR passes a writable buffer of `p2` wide chars.
                let buffer =
                    unsafe { std::slice::from_raw_parts_mut(p1 as *mut WideChar, p2 as usize) };
                buffer[..password.len()].copy_from_slice(password);
                buffer[password.len()] = 0;
                1
            }
            _ => -1,
        },
        // The narrow request only follows a declined wide one.
        native::UCM_NEEDPASSWORD => -1,
        _ => 0,
    }))
    .unwrap_or(-1)
}

/// Paths as unRAR takes them: wide strings, except on Linux/NetBSD where the
/// wide API mangles non-ASCII names and the narrow one is used (as the
/// `unrar` crate does).
#[cfg(any(target_os = "linux", target_os = "netbsd"))]
mod os {
    use super::*;
    use std::ffi::CString;

    pub(super) fn rar_string(path: &Path) -> Option<CString> {
        CString::new(path.as_os_str().as_encoded_bytes()).ok()
    }

    /// # Safety
    /// `handle` must be open and positioned after a header.
    pub(super) unsafe fn process_file(
        handle: *const native::Handle,
        operation: c_int,
        dest: Option<&CString>,
    ) -> c_int {
        unsafe {
            native::RARProcessFile(
                handle,
                operation,
                std::ptr::null(),
                dest.map_or(std::ptr::null(), |d| d.as_ptr()),
            )
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
mod os {
    use super::*;

    pub(super) fn rar_string(path: &Path) -> Option<WideCString> {
        WideCString::from_os_str(path).ok()
    }

    /// # Safety
    /// `handle` must be open and positioned after a header.
    pub(super) unsafe fn process_file(
        handle: *const native::Handle,
        operation: c_int,
        dest: Option<&WideCString>,
    ) -> c_int {
        unsafe {
            native::RARProcessFileW(
                handle,
                operation,
                std::ptr::null(),
                dest.map_or(std::ptr::null(), |d| d.as_ptr().cast()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processing::test_rar5 as rar5;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// unRAR decompresses a member it skips in a solid archive without ever
    /// calling back, so the skip must be done in a way that sees a stop.
    #[test]
    fn a_stop_is_seen_while_skipping_a_member_of_a_solid_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive = rar5::Rar5::new()
            .solid()
            .file("first.bin", &[7u8; 64 * 1024])
            .file("second.bin", b"second")
            .build();
        let path = write(dir.path(), "solid.rar", &archive);

        let cancel = Arc::new(AtomicBool::new(false));
        let lock = lock_unrar(&cancel).unwrap();
        // Unstopped, skipping reads through the member and reaches the next.
        let mut rar = RarArchive::open(&path, Mode::Extract, None, cancel.clone(), &lock).unwrap();
        assert!(rar.is_solid());
        rar.read_header().unwrap().expect("first member");
        assert_eq!(rar.skip(), Ok(()));
        let second = rar.read_header().unwrap().expect("second member");
        assert_eq!(second.name, PathBuf::from("second.bin"));
        drop(rar);

        // Stopped: the skip ends on the first chunk.
        let mut rar = RarArchive::open(&path, Mode::Extract, None, cancel.clone(), &lock).unwrap();
        rar.read_header().unwrap().expect("first member");
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(rar.skip(), Err(RarError::Cancelled));
    }

    /// Listing never unpacks, so moving to the next volume is the only time
    /// unRAR calls back: a stop is seen there too.
    #[test]
    fn a_stop_is_seen_when_unrar_moves_to_the_next_volume() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        let first = rar5::Rar5::new()
            .volume(0, true)
            .part("big.bin", &data[..1000], false, true, &data)
            .build();
        let second = rar5::Rar5::new()
            .volume(1, false)
            .part("big.bin", &data[1000..], true, false, &data)
            .build();
        let path = write(dir.path(), "set.part1.rar", &first);
        write(dir.path(), "set.part2.rar", &second);

        let cancel = Arc::new(AtomicBool::new(false));
        let lock = lock_unrar(&cancel).unwrap();
        // Unstopped, the listing goes through both volumes: one file.
        let mut rar = RarArchive::open(&path, Mode::List, None, cancel.clone(), &lock).unwrap();
        let entry = rar.read_header().unwrap().expect("the split file");
        assert_eq!(entry.unpacked_size, 1000);
        assert_eq!(rar.skip(), Ok(()));
        assert!(rar.read_header().unwrap().is_none());
        drop(rar);

        // Stopped: the move to the second volume ends it.
        let mut rar = RarArchive::open(&path, Mode::List, None, cancel.clone(), &lock).unwrap();
        rar.read_header().unwrap().expect("the split file");
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(rar.skip(), Err(RarError::Cancelled));
    }

    #[test]
    fn links_and_copies_are_reported_with_their_targets() {
        let dir = tempfile::tempdir().unwrap();
        let archive = rar5::Rar5::new()
            .file("data.txt", b"x")
            .link("hard", rar5::HARDLINK, "data.txt")
            .link("soft", rar5::UNIX_SYMLINK, "data.txt")
            .link("copy", rar5::FILE_COPY, "Sub/data.txt")
            .build();
        let path = write(dir.path(), "links.rar", &archive);
        let cancel = Arc::new(AtomicBool::new(false));
        let lock = lock_unrar(&cancel).unwrap();
        let mut rar = RarArchive::open(&path, Mode::List, None, cancel, &lock).unwrap();
        let mut seen = Vec::new();
        while let Some(entry) = rar.read_header().unwrap() {
            seen.push((entry.name.to_string_lossy().into_owned(), entry.redirect));
            rar.skip().unwrap();
        }
        assert_eq!(
            seen,
            vec![
                ("data.txt".into(), Redirect::None),
                ("hard".into(), Redirect::Link),
                ("soft".into(), Redirect::Link),
                ("copy".into(), Redirect::Copy(PathBuf::from("Sub/data.txt"))),
            ]
        );
    }

    /// A job waiting for another job's unRAR gives up as soon as it is
    /// stopped, and gets unRAR once the other is done with it.
    #[test]
    fn waiting_for_unrar_ends_on_a_stop_or_when_it_is_free() {
        let other = Arc::new(AtomicBool::new(false));
        let held = lock_unrar(&other).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let waiter = {
            let stop = stop.clone();
            std::thread::spawn(move || lock_unrar(&stop).is_some())
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !waiter.is_finished(),
            "took unRAR while another job held it"
        );
        stop.store(true, Ordering::Relaxed);
        assert!(!waiter.join().unwrap(), "a stopped job stops waiting");

        let free = Arc::new(AtomicBool::new(false));
        let waiter = {
            let free = free.clone();
            std::thread::spawn(move || lock_unrar(&free).is_some())
        };
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        assert!(waiter.join().unwrap(), "gets unRAR once it is free");
    }

    #[test]
    fn passwords_too_long_for_unrar_are_refused_not_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "plain.rar",
            &rar5::Rar5::new().file("a", b"a").build(),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let lock = lock_unrar(&cancel).unwrap();
        let longest = "p".repeat(MAX_PASSWORD_CHARS);
        assert!(
            RarArchive::open(&path, Mode::Extract, Some(&longest), cancel.clone(), &lock).is_ok()
        );
        let too_long = "p".repeat(MAX_PASSWORD_CHARS + 1);
        assert_eq!(
            RarArchive::open(&path, Mode::Extract, Some(&too_long), cancel, &lock).err(),
            Some(RarError::Code(Code::BadPassword))
        );
        assert_eq!(Password::new("é漢").wide_len(), 2);
    }

    #[test]
    fn a_password_is_wiped() {
        let mut password = Password::new("secret");
        password.wipe();
        assert_eq!(password.as_str().as_bytes(), &[0u8; 6]);
        let mut wide: Vec<WideChar> = vec![1, 2, 3];
        wipe(&mut wide);
        assert_eq!(wide, vec![0, 0, 0]);
    }
}
