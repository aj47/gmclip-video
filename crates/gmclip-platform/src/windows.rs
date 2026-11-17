//! The Windows FFI gmclip needs outside the GPU (decisions/0019): owner-only security
//! descriptors, the owner and DACL of a path, process identity (token user, image path,
//! parent), named-pipe peers, and free disk space.
//!
//! Every `unsafe` block here carries a `// SAFETY:` comment. Handles are wrapped in
//! [`OwnedHandle`] as soon as they exist, and every buffer the OS allocates with
//! `LocalAlloc` is freed by a guard, so no early return leaks one.

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetDiskFreeSpaceExW, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

use crate::sddl::{self, Object};

/// `s` as a NUL-terminated wide string; an interior NUL is refused (it would truncate
/// the name the OS sees).
fn wide(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut v: Vec<u16> = s.encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a path or name with a NUL in it",
        ));
    }
    v.push(0);
    Ok(v)
}

/// Owns a `LocalAlloc`ed pointer the OS handed back; `LocalFree` on drop.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to allocate it with
            // LocalAlloc (ConvertString*/ConvertSid*/GetSecurityInfo) and is freed once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Reads a NUL-terminated wide string the OS returned.
///
/// # Safety
/// `p` is null or points at a NUL-terminated UTF-16 string that stays valid for the call.
unsafe fn from_wide_nul(p: *const u16) -> OsString {
    if p.is_null() {
        return OsString::new();
    }
    let mut n = 0usize;
    // SAFETY: the caller guarantees NUL termination; we stop at the first NUL.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` u16s before the NUL are initialised and readable (above).
    OsString::from_wide(unsafe { std::slice::from_raw_parts(p, n) })
}

/// Wraps a handle the OS just returned, or the last error when it is null/invalid.
fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a fresh, valid handle that nothing else owns or closes.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

/// The string form of a SID.
///
/// # Safety
/// `sid` points at a valid SID for the duration of the call.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is valid (caller); `out` receives a LocalAlloc'ed string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = Local(out.cast());
    // SAFETY: on success `out` is a NUL-terminated string, alive until the guard drops.
    let s = unsafe { from_wide_nul(out) };
    s.into_string()
        .map_err(|_| io::Error::other("a SID that is not text"))
}

/// The user SID of a process token.
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is a valid process handle with query access; `token` is written.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = owned(token)?;
    let mut len = 0u32;
    // SAFETY: a size query: null buffer, zero length; only `len` is written.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut len,
        )
    };
    let err = io::Error::last_os_error();
    if ok != 0 || err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || len == 0 {
        return Err(err);
    }
    // u64 storage: TOKEN_USER holds pointers, so the buffer must be pointer-aligned
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `len` bytes, aligned for TOKEN_USER, and writable.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with an initialised TOKEN_USER whose SID
    // pointer points into the same buffer, which outlives this read.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: the SID lives inside `buf`, alive for the call.
    unsafe { sid_string(user.User.Sid) }
}

/// Opens a process for limited queries (works across integrity levels of one user).
fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: plain call; the result is checked and owned.
    owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
}

/// The SID of the user this process runs as (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    // SAFETY: the pseudo handle needs no closing and is always valid.
    token_user_sid(unsafe { GetCurrentProcess() })
}

/// The SID of the user process `pid` runs as.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    let p = open_process(pid)?;
    token_user_sid(p.as_raw_handle())
}

/// Does process `pid` run as this user? `false` when it cannot be asked.
pub fn process_is_current_user(pid: u32) -> bool {
    match (process_user_sid(pid), current_user_sid()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The executable of process `pid`, from the kernel (`QueryFullProcessImageNameW`, Win32
/// form), never its command line.
pub fn process_image(pid: u32) -> io::Result<PathBuf> {
    let p = open_process(pid)?;
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` holds `len` u16s; on success `len` is set to the chars written.
    if unsafe { QueryFullProcessImageNameW(p.as_raw_handle(), 0, buf.as_mut_ptr(), &mut len) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(PathBuf::from(OsString::from_wide(&buf)))
}

/// The parent process id of `pid` (a Toolhelp snapshot). Windows does not keep parents
/// alive, so the id may since have been reused: callers treat it as a hint for logs and
/// labels, never as an authority.
pub fn parent_pid(pid: u32) -> io::Result<u32> {
    // SAFETY: plain call; the snapshot handle is checked and owned.
    let snap = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) })?;
    // SAFETY: PROCESSENTRY32W is plain data; all-zero is a valid value.
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0);
    // SAFETY: `snap` is a process snapshot and `e.dwSize` is set as the API requires.
    let mut more = unsafe { Process32FirstW(snap.as_raw_handle(), &mut e) } != 0;
    while more {
        if e.th32ProcessID == pid {
            return Ok(e.th32ParentProcessID);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap.as_raw_handle(), &mut e) } != 0;
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no such process in the snapshot",
    ))
}

/// A parsed self-relative security descriptor plus the SECURITY_ATTRIBUTES pointing at
/// it; both live as long as this value.
struct OwnerOnly {
    sd: Local,
    sa: SECURITY_ATTRIBUTES,
}

impl OwnerOnly {
    fn new(object: Object) -> io::Result<OwnerOnly> {
        let sid = current_user_sid()?;
        let text = sddl::owner_only(&sid, object)
            .ok_or_else(|| io::Error::other("the current user's SID is not a SID string"))?;
        let w = wide(OsStr::new(&text))?;
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated; `sd` receives a LocalAlloc'ed descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sd = Local(sd);
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        Ok(OwnerOnly { sd, sa })
    }

    fn attrs(&self) -> *const SECURITY_ATTRIBUTES {
        debug_assert_eq!(self.sa.lpSecurityDescriptor, self.sd.0);
        &self.sa
    }
}

/// Creates one directory whose DACL lets only this user in (protected, inherited by what
/// is created inside), owned by this user. `AlreadyExists` if it exists (the existing
/// one is not changed: check it with [`file_security`]).
pub fn create_dir_owner_only(path: &Path) -> io::Result<()> {
    let sa = OwnerOnly::new(Object::Directory)?;
    let w = wide(path.as_os_str())?;
    // SAFETY: `w` is NUL-terminated; `sa` (and its descriptor) outlive the call.
    if unsafe { CreateDirectoryW(w.as_ptr(), sa.attrs()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Creates (never opens or replaces) a file only this user can open, read+write.
pub fn create_file_owner_only(path: &Path) -> io::Result<std::fs::File> {
    let sa = OwnerOnly::new(Object::File)?;
    let w = wide(path.as_os_str())?;
    // SAFETY: `w` is NUL-terminated; `sa` outlives the call; the handle is checked.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ,
            sa.attrs(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    Ok(std::fs::File::from(owned(h)?))
}

/// Who owns a path, and its DACL as SDDL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSecurity {
    pub owner_sid: String,
    pub dacl_sddl: String,
}

impl FileSecurity {
    /// Owned by this user and nobody else let in.
    pub fn is_private_to(&self, sid: &str) -> bool {
        self.owner_sid == sid && sddl::dacl_private_to(&self.dacl_sddl, sid)
    }
}

/// The owner and DACL of `path`. `follow: false` reads a symlink or junction itself
/// (`FILE_FLAG_OPEN_REPARSE_POINT`), as `lstat` would.
pub fn file_security(path: &Path, follow: bool) -> io::Result<FileSecurity> {
    let w = wide(path.as_os_str())?;
    let flags = FILE_FLAG_BACKUP_SEMANTICS
        | if follow {
            0
        } else {
            FILE_FLAG_OPEN_REPARSE_POINT
        };
    // SAFETY: `w` is NUL-terminated; READ_CONTROL only; the handle is checked and owned.
    let h = owned(unsafe {
        CreateFileW(
            w.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    })?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `h` is open with READ_CONTROL; `owner` points into `sd`, which is
    // LocalAlloc'ed and freed by its guard after both reads below.
    let rc = unsafe {
        GetSecurityInfo(
            h.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let sd = Local(sd);
    // SAFETY: `owner` points into the live descriptor `sd`.
    let owner_sid = unsafe { sid_string(owner) }?;
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sd.0` is a valid descriptor; `text` receives a LocalAlloc'ed string.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _text_guard = Local(text.cast());
    // SAFETY: on success `text` is NUL-terminated, alive until its guard drops.
    let dacl_sddl = unsafe { from_wide_nul(text) }
        .into_string()
        .map_err(|_| io::Error::other("SDDL that is not text"))?;
    Ok(FileSecurity {
        owner_sid,
        dacl_sddl,
    })
}

/// Bytes free to this user on the volume holding `path` (quotas included).
pub fn disk_free_bytes(path: &Path) -> io::Result<u64> {
    let w = wide(path.as_os_str())?;
    let mut avail = 0u64;
    // SAFETY: `w` is NUL-terminated; only `avail` is requested (the others may be null).
    if unsafe {
        GetDiskFreeSpaceExW(
            w.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(avail)
}

/// Fills `buf` from the system-preferred RNG (`BCryptGenRandom`), the Windows
/// counterpart of `/dev/urandom`.
pub fn fill_random(buf: &mut [u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    // one call takes at most u32::MAX bytes
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        let len = u32::try_from(chunk.len()).map_err(io::Error::other)?;
        // SAFETY: `chunk` is a valid, writable buffer of `len` bytes; with
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG the algorithm handle must be null.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                len,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed (NTSTATUS {status:#x})"
            )));
        }
    }
    Ok(())
}

/// Ctrl-Break to the console process group `group` (a process started with
/// `CREATE_NEW_PROCESS_GROUP`; its id is the group's): the Windows SIGTERM to a group,
/// which ends console programs that do not handle it. Fails when this process has no
/// console to share with the group (a GUI launcher), where there is no polite stop.
pub fn ctrl_break(group: u32) -> io::Result<()> {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: plain call with a group id; no pointers.
    if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The app's single instance on Windows (macOS's LaunchServices keeps one): a named
/// mutex, held for the process's life. `None` when another process holds it.
#[derive(Debug)]
pub struct Instance(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a mutex handle is a kernel object id, usable from any thread.
unsafe impl Send for Instance {}
// SAFETY: as above; the handle is only closed on drop.
unsafe impl Sync for Instance {}

impl Instance {
    /// Takes the instance named `name` (letters, digits, `-`, `_`; the session's own
    /// namespace), or `None` if a running process has it.
    pub fn claim(name: &str) -> io::Result<Option<Instance>> {
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "instance name"));
        }
        let wide: Vec<u16> = format!(r"Local\{name}").encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call; no security attributes.
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: plain call, right after CreateMutexW on this thread.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: a handle this call opened.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
            return Ok(None);
        }
        Ok(Some(Instance(h)))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle `claim` opened, closed once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// Brings process `pid`'s visible top-level window to the front (restored if it was
/// minimised); `false` when it has none. Called by a second launch (the process the user
/// just started may hand the foreground to another).
pub fn focus_process_window(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
        SetForegroundWindow, ShowWindow,
    };
    struct Find {
        pid: u32,
        hwnd: HWND,
    }
    unsafe extern "system" fn each(hwnd: HWND, l: LPARAM) -> i32 {
        // SAFETY: `l` is the `Find` passed below, alive for the whole EnumWindows call.
        let find = unsafe { &mut *(l as *mut Find) };
        let mut owner = 0u32;
        // SAFETY: a window handle EnumWindows gave; `owner` is a local.
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        // SAFETY: as above.
        if owner == find.pid && unsafe { IsWindowVisible(hwnd) } != 0 {
            find.hwnd = hwnd;
            return 0;
        }
        1
    }
    let mut find = Find {
        pid,
        hwnd: std::ptr::null_mut(),
    };
    // SAFETY: the callback reads `find` through the LPARAM only during this call.
    unsafe { EnumWindows(Some(each), &mut find as *mut Find as LPARAM) };
    if find.hwnd.is_null() {
        return false;
    }
    // SAFETY: a live top-level window handle found above.
    unsafe {
        if IsIconic(find.hwnd) != 0 {
            ShowWindow(find.hwnd, SW_RESTORE);
        }
        SetForegroundWindow(find.hwnd) != 0
    }
}

/// While held, this process asks for Windows' 1 ms timer (`timeBeginPeriod(1)`), as media
/// players do while they play: thread waits with a timeout (`recv_timeout`, parking) are
/// otherwise rounded to the 15.6 ms system tick, which drops frames at 60 fps. Released on
/// drop (`timeEndPeriod`); nothing persists after the process.
#[derive(Debug)]
pub struct FineTimer(());

impl FineTimer {
    pub fn begin() -> Self {
        // SAFETY: plain call; balanced by the `timeEndPeriod(1)` in `Drop`.
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
        Self(())
    }
}

impl Drop for FineTimer {
    fn drop(&mut self) {
        // SAFETY: plain call, balancing `begin`'s `timeBeginPeriod(1)`.
        unsafe { windows_sys::Win32::Media::timeEndPeriod(1) };
    }
}

/// Keeps the system awake while held (macOS's `caffeinate -i`): a power request
/// (`PowerRequestSystemRequired`, as Windows' own media apps make), named so
/// `powercfg /requests` shows why. The display may still sleep. Tied to its handle, not
/// to a thread; released on drop, and by Windows if the process ends.
#[derive(Debug)]
pub struct PowerRequest(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a power request handle is a kernel object id, usable from any thread.
unsafe impl Send for PowerRequest {}
// SAFETY: as above; the handle is only cleared and closed on drop.
unsafe impl Sync for PowerRequest {}

impl PowerRequest {
    /// Holds the system awake, with `reason` shown to the user (`powercfg /requests`).
    pub fn system_required(reason: &str) -> io::Result<PowerRequest> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Power::{
            PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
        };
        use windows_sys::Win32::System::Threading::{
            POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
        };
        let mut wide: Vec<u16> = reason.encode_utf16().chain([0]).collect();
        let ctx = REASON_CONTEXT {
            // POWER_REQUEST_CONTEXT_VERSION
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: wide.as_mut_ptr(),
            },
        };
        // SAFETY: `ctx` and the NUL-terminated `wide` it points at outlive the call (the
        // request copies the string).
        let h = unsafe { PowerCreateRequest(&ctx) };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `h` is the request just made.
        if unsafe { PowerSetRequest(h, PowerRequestSystemRequired) } == 0 {
            let e = io::Error::last_os_error();
            // SAFETY: closes the request made above, once.
            unsafe { CloseHandle(h) };
            return Err(e);
        }
        Ok(PowerRequest(h))
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Power::{PowerClearRequest, PowerRequestSystemRequired};
        // SAFETY: the request `system_required` made and set, cleared and closed once.
        unsafe {
            PowerClearRequest(self.0, PowerRequestSystemRequired);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Stops this process's standard handles from being inherited by processes it starts
/// from now on. Windows hands a child every inheritable handle (Unix closes them on exec):
/// a long-lived child (the `gmclip mcp` shim's detached holder) would otherwise hold the
/// shim's stdio pipes open, and its client would never see them close. The handles keep
/// working here; a child's own stdio is still given to it explicitly.
pub fn keep_std_handles_to_self() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle takes one of the three ids and returns a handle (or null /
        // INVALID_HANDLE_VALUE, skipped); SetHandleInformation only clears its inherit
        // flag (a failure, as on a console pseudo-handle, leaves it as it was).
        unsafe {
            let h = GetStdHandle(id);
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// The Windows file dialog (W.9; `GetOpenFileNameW`, the plain C API, so no COM): an
/// existing file of the kinds `exts` names, or several with `multi`. Empty when the
/// person cancels. Runs modally on the calling thread (the app calls it on its main one).
pub fn open_file_dialog(
    title: &str,
    filter_name: &str,
    exts: &[&str],
    multi: bool,
) -> io::Result<Vec<PathBuf>> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER,
        OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let filter = crate::windialog::filter(filter_name, exts);
    let title = wide(OsStr::new(title))?;
    // room for many names when several may be chosen
    let mut file = vec![0u16; if multi { 1 << 16 } else { 1 << 15 }];
    // SAFETY: an all-zero OPENFILENAMEW is a valid start (null pointers, no hook); the
    // fields the dialog reads are set below.
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize =
        u32::try_from(std::mem::size_of::<OPENFILENAMEW>()).map_err(io::Error::other)?;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = u32::try_from(file.len()).map_err(io::Error::other)?;
    ofn.lpstrTitle = title.as_ptr();
    ofn.Flags = OFN_EXPLORER
        | OFN_FILEMUSTEXIST
        | OFN_PATHMUSTEXIST
        | OFN_NOCHANGEDIR
        | OFN_HIDEREADONLY
        | if multi { OFN_ALLOWMULTISELECT } else { 0 };
    // SAFETY: `ofn` points at the filter, title and file buffers above, which outlive
    // the call; `nMaxFile` is the file buffer's length in u16s.
    if unsafe { GetOpenFileNameW(&mut ofn) } == 0 {
        // SAFETY: no arguments; reads the calling thread's last dialog error.
        let e = unsafe { CommDlgExtendedError() };
        return if e == 0 {
            Ok(Vec::new()) // cancelled
        } else {
            Err(io::Error::other(format!(
                "the file dialog failed (error {e:#x})"
            )))
        };
    }
    Ok(crate::windialog::selection(&file)
        .into_iter()
        .map(PathBuf::from)
        .collect())
}

/// A job object for an agent run (W.9, decisions/0019): the launcher puts the client in
/// it, and everything the client starts is in it too, so what a client leaves behind
/// when it exits on its own (an MCP server holding the run's credential in its
/// environment) ends with the run. Kill-on-close: dropping the [`Job`] (the launcher's
/// only handle) ends every process still in it, and so does the launcher's own exit,
/// however it happens.
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    /// A new unnamed job whose processes all end when its last handle closes.
    pub fn kill_on_close() -> io::Result<Job> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // SAFETY: both arguments may be null (default security, no name); the returned
        // handle is owned at once (null on failure, which `owned` turns into an error).
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Job(owned(h)?);
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid value (no
        // limits); only the kill-on-close flag is then set.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let len = u32::try_from(std::mem::size_of_val(&info)).map_err(io::Error::other)?;
        // SAFETY: the handle is a live job handle; `info` is a valid struct of the class
        // named and `len` is its size.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts a running process in the job (a child the launcher spawned: its handle).
    /// Processes it starts afterwards are in the job too.
    pub fn assign(&self, process: &impl AsRawHandle) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are live for the call (the job is ours, the process
        // handle is borrowed from its owner).
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// How many processes are in the job now.
    pub fn active_processes(&self) -> io::Result<u32> {
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };
        // SAFETY: an all-zero accounting struct is a valid value for the OS to fill.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let len = u32::try_from(std::mem::size_of_val(&info)).map_err(io::Error::other)?;
        // SAFETY: the job handle is live; `info` is a writable struct of the class named,
        // `len` its size; the returned-length pointer may be null.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                len,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }

    /// Ends every process in the job now, with `code` as their exit code.
    pub fn terminate(&self, code: u32) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        // SAFETY: the job handle is live for the call.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Named pipes for the local MCP host (decisions/0019).
pub mod pipe {
    use super::*;

    /// Creates one instance of the pipe `name` (`\\.\pipe\…`), owner-only, local clients
    /// only, byte mode. `first: true` fails if any instance of that name exists already
    /// (`ERROR_ACCESS_DENIED`): another holder, or someone squatting the name. Needs a
    /// tokio runtime with I/O enabled.
    pub fn create_server(name: &OsStr, first: bool) -> io::Result<NamedPipeServer> {
        let sa = OwnerOnly::new(Object::Pipe)?;
        let mut opts = ServerOptions::new();
        opts.first_pipe_instance(first)
            .reject_remote_clients(true)
            .access_inbound(true)
            .access_outbound(true);
        // SAFETY: `sa.attrs()` points at a valid SECURITY_ATTRIBUTES (and descriptor)
        // that outlives the call; CreateNamedPipeW copies what it needs.
        unsafe { opts.create_with_security_attributes_raw(name, sa.attrs().cast_mut().cast()) }
    }

    /// The process id of the client connected to this server instance.
    pub fn client_pid(s: &NamedPipeServer) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: the handle is a live named-pipe server handle owned by `s`.
        if unsafe { GetNamedPipeClientProcessId(s.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pid)
    }

    /// The process id of the server this client is connected to.
    pub fn server_pid(c: &NamedPipeClient) -> io::Result<u32> {
        let mut pid = 0u32;
        // SAFETY: the handle is a live named-pipe client handle owned by `c`.
        if unsafe { GetNamedPipeServerProcessId(c.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmclip-platform-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// A process that outlives its parent ends with the job: `cmd` starts a detached
    /// `ping` (the way a client leaves an MCP server behind) and exits; dropping the job
    /// ends the `ping`. (Windows only: awaits the PC.)
    #[test]
    fn a_job_ends_what_a_child_left_behind() {
        let job = Job::kill_on_close().unwrap();
        let sys = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let mut c = std::process::Command::new(sys.join("System32").join("cmd.exe"))
            .args(["/c", "start", "/b", "ping", "-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        job.assign(&c).unwrap();
        c.wait().unwrap();
        // cmd is gone; its ping is still in the job
        let t = std::time::Instant::now();
        while job.active_processes().unwrap() == 0 && t.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            job.active_processes().unwrap() >= 1,
            "the ping should still run"
        );
        job.terminate(1).unwrap();
        let t = std::time::Instant::now();
        while job.active_processes().unwrap() > 0 && t.elapsed().as_secs() < 5 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(job.active_processes().unwrap(), 0);
        drop(job);
    }

    #[test]
    fn we_know_who_we_are() {
        let me = current_user_sid().unwrap();
        assert!(sddl::is_sid(&me), "{me}");
        assert_eq!(process_user_sid(std::process::id()).unwrap(), me);
        assert!(process_is_current_user(std::process::id()));
        let exe = process_image(std::process::id()).unwrap();
        assert_eq!(
            exe.canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
        assert!(parent_pid(std::process::id()).is_ok());
    }

    #[test]
    fn owner_only_dirs_and_files_are_private_and_never_reused() {
        let me = current_user_sid().unwrap();
        let d = tmp("acl");
        create_dir_owner_only(&d).unwrap();
        let sec = file_security(&d, false).unwrap();
        assert!(sec.is_private_to(&me), "{sec:?}");
        assert_eq!(
            create_dir_owner_only(&d).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let f = d.join("pairing");
        drop(create_file_owner_only(&f).unwrap());
        assert!(file_security(&f, false).unwrap().is_private_to(&me));
        assert!(create_file_owner_only(&f).is_err(), "never replaced");
        // a plain temp dir inherits the profile's ACL: not private by our rule
        let plain = tmp("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!file_security(&plain, false).unwrap().is_private_to(&me));
        assert!(disk_free_bytes(&d).unwrap() > 0);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn pipes_report_their_peer() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let name = format!(r"\\.\pipe\gmclip-platform-test-{}", std::process::id());
            let server = pipe::create_server(OsStr::new(&name), true).unwrap();
            // a second "first" instance of the same name is refused (no squatting)
            assert!(pipe::create_server(OsStr::new(&name), true).is_err());
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&name)
                .unwrap();
            server.connect().await.unwrap();
            assert_eq!(pipe::client_pid(&server).unwrap(), std::process::id());
            assert_eq!(pipe::server_pid(&client).unwrap(), std::process::id());
        });
    }
}
