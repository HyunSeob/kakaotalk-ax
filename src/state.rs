//! Private, atomic send journals, process locks, and one-way migration from Python v1 journals.

use std::cell::Cell;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{KaxError, Result};

pub const JOURNAL_SCHEMA_VERSION: u32 = 2;

/// Canonical state directory for the Rust binary.
/// - macOS/Unix: `~/.config/kakaotalk-ax` (migrates one-way from `~/.config/kakaotalk-reader`)
/// - Windows: `%LOCALAPPDATA%\kakaotalk-ax` (upgrades legacy v1 records in-place)
pub fn state_directory() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("kakaotalk-ax");
            }
        }
        home_dir().join("AppData").join("Local").join("kakaotalk-ax")
    }
    #[cfg(not(target_os = "windows"))]
    {
        home_dir().join(".config").join("kakaotalk-ax")
    }
}

/// Legacy Python state directory for one-way migration.
pub fn legacy_state_directory() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        state_directory()
    }
    #[cfg(not(target_os = "windows"))]
    {
        home_dir().join(".config").join("kakaotalk-reader")
    }
}

pub fn default_ui_root() -> PathBuf {
    state_directory().join("ax-ui")
}

pub fn default_send_state_root() -> PathBuf {
    state_directory().join("ax-send-operations")
}

pub fn legacy_send_state_root() -> PathBuf {
    legacy_state_directory().join("ax-send-operations")
}

fn home_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(p) = std::env::var("USERPROFILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    PathBuf::from(".")
}

pub fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let out = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

/// Exact Python-compatible fingerprint:
/// `hashlib.sha256(json.dumps([window_title, text, require_self], ensure_ascii=False).encode("utf-8")).hexdigest()`
/// Python `json.dumps` uses `", "` separators by default!
pub fn send_fingerprint(window_title: &str, text: &str, require_self: bool) -> String {
    let wt = serde_json::to_string(window_title).unwrap_or_default();
    let tx = serde_json::to_string(text).unwrap_or_default();
    let rs = if require_self { "true" } else { "false" };
    let serialized = format!("[{wt}, {tx}, {rs}]");
    sha256_hex(&serialized)
}

/// Ensure directory exists with private permissions (`0700` on Unix).
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Open a regular state file without following a final symlink or Windows reparse point.
#[cfg(unix)]
pub fn open_private(path: &Path, writable: bool) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = fs::OpenOptions::new();
    opts.read(true)
        .write(writable)
        .create(writable)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600);
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if meta.is_dir() {
        return Err(std::io::Error::from_raw_os_error(libc::ELOOP));
    }
    Ok(file)
}

#[cfg(target_os = "windows")]
pub fn open_private(path: &Path, writable: bool) -> std::io::Result<File> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
        GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandleEx, FileAttributeTagInfo, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING,
    };

    #[repr(C)]
    struct FileAttrTagInfo {
        file_attributes: u32,
        reparse_tag: u32,
    }

    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let access = if writable {
        GENERIC_READ | GENERIC_WRITE
    } else {
        GENERIC_READ
    };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let disposition = if writable { OPEN_ALWAYS } else { OPEN_EXISTING };

    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = unsafe { GetLastError() };
        if err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                path.display().to_string(),
            ));
        }
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }

    let mut info = FileAttrTagInfo {
        file_attributes: 0,
        reparse_tag: 0,
    };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut info as *mut FileAttrTagInfo).cast(),
            std::mem::size_of::<FileAttrTagInfo>() as u32,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        unsafe { CloseHandle(handle) };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    if (info.file_attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)) != 0 {
        unsafe { CloseHandle(handle) };
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Unsafe state file: {}", path.display()),
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Atomically write JSON to a private file (`0600` on Unix, `MoveFileExW` on Windows).
pub fn save_private_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| KaxError::runtime("OSError", "Missing parent directory"))?;
    ensure_private_dir(parent)?;

    let mut temp = tempfile::Builder::new()
        .prefix(".operation-")
        .tempfile_in(parent)?;
    let bytes = serde_json::to_vec(value)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o600))?;
    }

    let temp_path = temp.into_temp_path();
    replace_private(&temp_path, path)?;
    Ok(())
}

#[cfg(unix)]
fn replace_private(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)?;
    if let Some(parent) = target.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn replace_private(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let src: Vec<u16> = OsStr::new(temporary)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst: Vec<u16> = OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let ok = unsafe {
        MoveFileExW(
            src.as_ptr(),
            dst.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        return Err(std::io::Error::from_raw_os_error(err as i32));
    }
    Ok(())
}

pub fn load_private_json(path: &Path) -> Result<Option<Value>> {
    match open_private(path, false) {
        Ok(mut file) => {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            let val: Value = serde_json::from_str(&text)?;
            Ok(Some(val))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Exclusive non-blocking cross-process file lock.
pub struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    pub fn acquire(root: &Path, busy_message: &str) -> Result<Self> {
        ensure_private_dir(root)?;
        let lock_path = root.join(".lock");
        let file = open_private(&lock_path, true)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(KaxError::validation(busy_message));
                }
                return Err(err.into());
            }
        }

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{
                GetLastError, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
            };
            use windows_sys::Win32::Storage::FileSystem::{
                LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            };
            use windows_sys::Win32::System::IO::OVERLAPPED;

            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            let ok = unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                if err == ERROR_LOCK_VIOLATION || err == ERROR_SHARING_VIOLATION {
                    return Err(KaxError::validation(busy_message));
                }
                return Err(std::io::Error::from_raw_os_error(err as i32).into());
            }
        }

        Ok(Self { _file: file })
    }
}

pub fn send_lock(root: &Path) -> Result<ProcessLock> {
    ProcessLock::acquire(root, "Another KakaoTalk send is in progress")
}

// Re-entrant UI session lock matching Python's `ui_session()` (`_mutex` + thread-local depth + `process_lock`).
static UI_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

thread_local! {
    static UI_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub struct UiSessionGuard {
    _lock: Option<ProcessLock>,
    _mutex_guard: Option<std::sync::MutexGuard<'static, ()>>,
}

impl Drop for UiSessionGuard {
    fn drop(&mut self) {
        UI_DEPTH.with(|d| {
            let cur = d.get();
            if cur > 0 {
                d.set(cur - 1);
            }
        });
    }
}

pub fn ui_session(ui_root: &Path) -> Result<UiSessionGuard> {
    let already_held = UI_DEPTH.with(|d| d.get() > 0);
    if already_held {
        UI_DEPTH.with(|d| d.set(d.get() + 1));
        return Ok(UiSessionGuard {
            _lock: None,
            _mutex_guard: None,
        });
    }

    let mutex = UI_MUTEX.get_or_init(|| Mutex::new(()));
    let guard = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lock = ProcessLock::acquire(ui_root, "Another KakaoTalk AX operation is in progress")?;
    UI_DEPTH.with(|d| d.set(1));
    Ok(UiSessionGuard {
        _lock: Some(lock),
        _mutex_guard: Some(guard),
    })
}

/// Summary report of a one-way legacy Python -> Rust send journal migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub status: String,
    pub source_dir: String,
    pub target_dir: String,
    pub migrated: usize,
    pub skipped_existing: usize,
    pub upgraded_in_place: usize,
    pub invalid_files: usize,
}

/// One-way migration of legacy Python v1 send journals into the Rust v2 journal store.
///
/// - Validates each `<64-hex>.json` record has the required fields (`operationId`, `windowTitle`,
///   `fingerprint`, `textHash`, `baselineTailHashes`, `baselineTextCount`, `phase`).
/// - Preserves `operationId`, `windowTitle`, `fingerprint`, `textHash`, `baselineTailHashes`,
///   `baselineTextCount`, `createdAt`, `phase`, `reason`, `native`, `windowIdentity`.
/// - Marks migrated records with `"schemaVersion": 2` and `"migratedFrom": "python_v1"`.
/// - Never overwrites an already-migrated v2 record in `target_dir`.
/// - Writes a `.migrated-v2` marker file in `target_dir` once completed.
pub fn migrate_legacy_journal(source_dir: &Path, target_dir: &Path) -> Result<MigrationReport> {
    ensure_private_dir(target_dir)?;
    let _lock = send_lock(target_dir)?;
    migrate_legacy_journal_unlocked(source_dir, target_dir)
}

pub(crate) fn migrate_legacy_journal_unlocked(
    source_dir: &Path,
    target_dir: &Path,
) -> Result<MigrationReport> {
    ensure_private_dir(target_dir)?;
    let same_dir = source_dir == target_dir
        || (source_dir.exists()
            && target_dir.exists()
            && fs::canonicalize(source_dir).ok() == fs::canonicalize(target_dir).ok());

    let mut report = MigrationReport {
        status: "ok".to_string(),
        source_dir: source_dir.display().to_string(),
        target_dir: target_dir.display().to_string(),
        migrated: 0,
        skipped_existing: 0,
        upgraded_in_place: 0,
        invalid_files: 0,
    };

    if !source_dir.is_dir() {
        write_migration_marker(target_dir)?;
        return Ok(report);
    }

    let entries = fs::read_dir(source_dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".json") || file_name.starts_with('.') {
            continue;
        }
        let stem = &file_name[..file_name.len() - 5];
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }

        let record = match load_private_json(&path) {
            Ok(Some(Value::Object(obj))) => obj,
            _ => {
                report.invalid_files += 1;
                continue;
            }
        };

        if !is_valid_journal_record(&record) {
            report.invalid_files += 1;
            continue;
        }

        let already_v2 = record
            .get("schemaVersion")
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v >= JOURNAL_SCHEMA_VERSION as u64);

        let dest = target_dir.join(file_name);
        if same_dir {
            if already_v2 {
                report.skipped_existing += 1;
                continue;
            }
            let upgraded = upgrade_v1_record(record);
            save_private_json(&dest, &Value::Object(upgraded))?;
            report.upgraded_in_place += 1;
            report.migrated += 1;
        } else {
            if dest.exists() {
                report.skipped_existing += 1;
                continue;
            }
            let upgraded = if already_v2 {
                record
            } else {
                upgrade_v1_record(record)
            };
            save_private_json(&dest, &Value::Object(upgraded))?;
            report.migrated += 1;
        }
    }

    write_migration_marker(target_dir)?;
    Ok(report)
}

fn is_valid_journal_record(record: &Map<String, Value>) -> bool {
    let has_str = |k: &str| record.get(k).and_then(|v| v.as_str()).is_some();
    has_str("operationId")
        && has_str("windowTitle")
        && has_str("fingerprint")
        && has_str("textHash")
        && has_str("phase")
        && record
            .get("baselineTailHashes")
            .and_then(|v| v.as_array())
            .is_some()
        && record
            .get("baselineTextCount")
            .and_then(|v| v.as_u64())
            .is_some()
}

fn upgrade_v1_record(mut record: Map<String, Value>) -> Map<String, Value> {
    record.insert(
        "schemaVersion".to_string(),
        Value::Number(JOURNAL_SCHEMA_VERSION.into()),
    );
    record.insert(
        "migratedFrom".to_string(),
        Value::String("python_v1".to_string()),
    );
    record
}

fn write_migration_marker(target_dir: &Path) -> Result<()> {
    let marker = target_dir.join(".migrated-v2");
    if !marker.exists() {
        let payload = serde_json::json!({
            "schemaVersion": JOURNAL_SCHEMA_VERSION,
        });
        save_private_json(&marker, &payload)?;
    }
    Ok(())
}

/// Automatically runs the one-way migration once when using the default send state root.
pub fn ensure_migrated_if_default(target_dir: &Path) -> Result<()> {
    let default_target = default_send_state_root();
    if target_dir != default_target {
        return Ok(());
    }
    let marker = target_dir.join(".migrated-v2");
    if marker.exists() {
        return Ok(());
    }
    let legacy = legacy_send_state_root();
    let _ = migrate_legacy_journal_unlocked(&legacy, target_dir)?;
    Ok(())
}
