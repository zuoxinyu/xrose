//! Windows job object that ties core processes to the app's lifetime.
//!
//! Windows does not terminate children when their parent dies, so killing the
//! GUI (crash, Task Manager, closed terminal) used to leave `sing-box` running
//! with its `cache.db` exclusively locked. The next launch then looped on
//! `FATAL: start service: initialize cache-file: timeout`. Every core spawned
//! by the supervisor is adopted into a `KILL_ON_JOB_CLOSE` job; the OS closes
//! the last job handle with the app and reaps the core with it.
//!
//! Adoption is best effort: if the app itself already runs inside a restrictive
//! job, `AssignProcessToJobObject` can fail. The core still runs normally; it
//! just loses the crash-time cleanup guarantee.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

/// Raw job handle, kept open for the whole process lifetime.
static JOB: OnceLock<usize> = OnceLock::new();
static WARNED: AtomicBool = AtomicBool::new(false);

fn job() -> Option<HANDLE> {
    let raw = *JOB.get_or_init(|| {
        // SAFETY: called once; a null security descriptor/name is the documented
        // way to create an unnamed job with default security.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            unsafe { CloseHandle(handle) };
            return 0;
        }
        handle as usize
    });
    (raw != 0).then_some(raw as HANDLE)
}

/// Adopts a freshly spawned core process. Returns an error instead of failing
/// the start: losing crash-time cleanup is not a reason to refuse to run.
pub fn adopt(process: HANDLE) -> Result<(), String> {
    let job = job().ok_or_else(|| "无法创建 Windows 作业对象".to_string())?;
    // SAFETY: both handles are valid for the call; the process handle comes from
    // the live `tokio::process::Child`.
    let ok = unsafe { AssignProcessToJobObject(job, process) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// True the first time a caller observes an adoption failure, so the warning is
/// logged once instead of on every watchdog restart.
pub fn take_warning() -> bool {
    !WARNED.swap(true, Ordering::SeqCst)
}
