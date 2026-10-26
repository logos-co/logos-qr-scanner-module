//! What each system needs before the camera opens.
//!
//! macOS charges a process's camera to the app responsible for it: Basecamp for a module there, the
//! terminal for one under logosctl. So the helper first re-executes itself with that link
//! disclaimed, as Qt Creator's `disclaim` and Chromium do, and macOS then asks in the helper's own
//! name. It keeps its process id and its pipes. Then it asks for the camera itself, since SDL relies
//! on an implicit prompt and reports a refusal only as a failure to open.

#[cfg(target_os = "macos")]
pub const DENIED_HINT: &str =
    "camera access is off for Logos QR: turn it on in System Settings > Privacy & Security > Camera";
#[cfg(target_os = "windows")]
pub const DENIED_HINT: &str =
    "camera access is off: turn on \"Let desktop apps access your camera\" in Settings > Privacy & security > Camera";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const DENIED_HINT: &str =
    "the camera can't be opened: the user needs access to /dev/video* (the video group)";

#[derive(Debug, PartialEq, Eq)]
// Only macOS asks ahead; elsewhere the camera is refused when it opens.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Access {
    Granted,
    Denied,
    Restricted,
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, OsString};
    use std::os::raw::{c_char, c_int};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr::null_mut;
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeVideo};

    use super::Access;

    const MARK: &str = "LOGOS_QR_DISCLAIMED";
    // <spawn.h>: replace the calling process instead of creating a child.
    const POSIX_SPAWN_SETEXEC: libc::c_short = 0x0040;

    extern "C" {
        fn _NSGetEnviron() -> *mut *mut *mut c_char;
    }

    /// Re-execute this process with its responsibility disclaimed, once. Returns only when that
    /// isn't possible, and says why on standard error.
    pub fn disclaim() {
        if std::env::var_os(MARK).is_some() {
            return;
        }
        std::env::set_var(MARK, "1");
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name; a missing symbol is null.
        let sym = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_spawnattrs_setdisclaim".as_ptr(),
            )
        };
        if sym.is_null() {
            eprintln!("logos-qr-helper: this macOS has no responsibility_spawnattrs_setdisclaim; the camera prompt names the app that started the helper");
            return;
        }
        type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, c_int) -> c_int;
        // SAFETY: the symbol has this signature in libSystem since macOS 10.14 (LLDB, Chromium).
        let set_disclaim: SetDisclaim = unsafe { std::mem::transmute(sym) };
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let Ok(path) = CString::new(exe.as_os_str().as_bytes()) else {
            return;
        };
        let args: Vec<CString> = std::env::args_os()
            .map(|a: OsString| CString::new(a.as_bytes()).unwrap_or_default())
            .collect();
        let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
        argv.push(null_mut());
        // SAFETY: the attribute is initialised before use and only freed if the exec fails; on
        // success this process image is replaced and nothing after posix_spawn runs.
        unsafe {
            let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
            if libc::posix_spawnattr_init(&mut attr) != 0 {
                return;
            }
            libc::posix_spawnattr_setflags(&mut attr, POSIX_SPAWN_SETEXEC);
            if set_disclaim(&mut attr, 1) != 0 {
                eprintln!("logos-qr-helper: disclaiming failed; the camera prompt names the app that started the helper");
                libc::posix_spawnattr_destroy(&mut attr);
                return;
            }
            let rc = libc::posix_spawn(
                null_mut(),
                path.as_ptr(),
                null_mut(),
                &attr,
                argv.as_ptr(),
                *_NSGetEnviron(),
            );
            libc::posix_spawnattr_destroy(&mut attr);
            eprintln!("logos-qr-helper: re-executing disclaimed failed ({rc}); the camera prompt names the app that started the helper");
        }
    }

    pub fn camera_access() -> Access {
        // SAFETY: AVMediaTypeVideo is a framework constant; the completion block only sends on a channel.
        unsafe {
            let Some(video) = AVMediaTypeVideo else {
                return Access::Restricted;
            };
            let status = AVCaptureDevice::authorizationStatusForMediaType(video);
            if status == AVAuthorizationStatus::Authorized {
                return Access::Granted;
            }
            if status == AVAuthorizationStatus::Denied {
                return Access::Denied;
            }
            if status == AVAuthorizationStatus::Restricted {
                return Access::Restricted;
            }
            let (tx, rx) = mpsc::channel();
            let done = RcBlock::new(move |granted: Bool| {
                let _ = tx.send(granted.as_bool());
            });
            AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &done);
            match rx.recv() {
                Ok(true) => Access::Granted,
                _ => Access::Denied,
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::{camera_access, disclaim};

#[cfg(not(target_os = "macos"))]
pub fn disclaim() {}

/// Elsewhere the camera's access is the system's to refuse when it opens.
#[cfg(not(target_os = "macos"))]
pub fn camera_access() -> Access {
    Access::Granted
}
