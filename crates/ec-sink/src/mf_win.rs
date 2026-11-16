//! Windows 11 output: switching on the "Icontact Camera" that meeting apps pick.
//!
//! Two parts make the Windows camera:
//!
//! - the camera library `icontact_mfcam.dll` (crate `ec-mfcam`), installed
//!   once with administrator rights by `scripts\install-windows.ps1`. Windows
//!   loads it inside its own camera service;
//! - this file: while the app runs, it asks Windows to list a camera backed by
//!   that library (`MFCreateVirtualCamera`). The camera exists only as long as
//!   the app does and needs no administrator rights to switch on.
//!
//! The pictures do not go through here. The app writes them into the shared
//! block ([`crate::Writer`]) and the library reads them there.
//!
//! WRITTEN ON A MAC AND ONLY COMPILE-CHECKED FOR WINDOWS. Not yet run.

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{s, w, Interface, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MFShutdown, MFStartup, MFVirtualCameraAccess,
    MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType, MFVirtualCameraType_SoftwareCameraSource, MFSTARTUP_FULL, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::{SinkError, WIN_CAMERA_CLSID, WIN_CAMERA_NAME};

/// Where the install script puts the camera library. A folder only
/// administrators can write to, because a Windows service loads the file.
pub fn library_path() -> PathBuf {
    let base = std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
    PathBuf::from(base)
        .join("Icontact")
        .join("icontact_mfcam.dll")
}

/// Has the one-time install been done? (Only looks for the file; the
/// registry entry is written by the same script.)
pub fn installed() -> bool {
    library_path().exists()
}

fn failed(what: &str, e: windows::core::Error) -> SinkError {
    SinkError::Camera(format!("{what} failed: {e}"))
}

/// The C signature of `MFCreateVirtualCamera`, to call it through a pointer.
type CreateFn = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut c_void,
) -> HRESULT;

/// The camera, switched on. Dropping it removes the camera from Windows.
///
/// It must be made and dropped on the same thread (the app's pipeline thread).
pub struct WinCamera {
    camera: IMFVirtualCamera,
}

impl WinCamera {
    /// Ask Windows to list "Icontact Camera" for this user until the app ends.
    pub fn start() -> Result<WinCamera, SinkError> {
        unsafe {
            // Media Foundation wants COM and itself started on this thread
            // first. "Already started" is fine, so that result is ignored.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| failed("starting Media Foundation", e))?;

            // `MFCreateVirtualCamera` exists on Windows 11 only. Looking it up
            // by name at run time (instead of linking to it) lets the app
            // still start on Windows 10 and say so plainly.
            let module = LoadLibraryW(w!("mfsensorgroup.dll"))
                .map_err(|e| failed("loading Windows' camera library", e))?;
            let Some(address) = GetProcAddress(module, s!("MFCreateVirtualCamera")) else {
                return Err(SinkError::Camera(
                    "this Windows has no built-in fake camera support (it needs Windows 11)".into(),
                ));
            };
            // SAFETY: the function with this name has this signature (Windows SDK, mfvirtualcamera.h).
            let create: CreateFn = std::mem::transmute(address);

            let name = HSTRING::from(WIN_CAMERA_NAME);
            let class = HSTRING::from(WIN_CAMERA_CLSID);
            let mut raw = std::ptr::null_mut();
            create(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session, // gone when the app is gone
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name.as_ptr()),
                PCWSTR(class.as_ptr()),
                std::ptr::null(),
                0,
                &mut raw,
            )
            .ok()
            .map_err(|e| failed("creating the camera", e))?;
            if raw.is_null() {
                return Err(SinkError::Camera("Windows returned no camera".into()));
            }
            // SAFETY: on success `raw` is a counted reference to the camera, now ours.
            let camera = IMFVirtualCamera::from_raw(raw);
            camera
                .Start(None)
                .map_err(|e| failed("starting the camera", e))?;
            Ok(WinCamera { camera })
        }
    }
}

impl Drop for WinCamera {
    fn drop(&mut self) {
        unsafe {
            // Take the camera off Windows' list again; nothing to do if a step fails.
            let _ = self.camera.Remove();
            let _ = self.camera.Shutdown();
            let _ = MFShutdown();
        }
    }
}
