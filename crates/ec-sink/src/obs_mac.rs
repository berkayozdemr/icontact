//! macOS output: pushing frames into OBS's installed camera.
//!
//! A Mac camera of our own would need a paid Apple developer account. OBS
//! already ships one ("OBS Virtual Camera"), and macOS lets any program hand
//! frames to it through Apple's public camera interface, CoreMediaIO. OBS only
//! has to be installed, with its camera started once so macOS approved it; it
//! does not have to be running. Meeting apps then pick "OBS Virtual Camera".
//!
//! The steps (worked out from pyvirtualcam's MIT-licensed
//! `native_macos_obs_cmioextension/virtual_output.hpp`; this is our own code):
//! 1. list the cameras macOS knows, find the one with OBS's fixed id;
//! 2. that camera has two streams: one that apps read, one that takes frames
//!    in. Ask the second for its queue and start it;
//! 3. per frame: put the picture into a macOS picture buffer (in the UYVY
//!    layout), wrap it with a time stamp, and put it on the queue.
//!
//! This file talks to macOS's C interfaces directly. Everything `unsafe` is
//! kept inside it; the rest of the app sees [`ObsCamera`], [`obs_state`] and
//! [`list_cameras`].
//!
//! STATE (2026-11-15, OBS 33.0.1, macOS 26.6.2): all three steps run. 299 of 300
//! numbered test pictures came back out of the camera intact, about 4 ms after
//! they went in (`ec-cli obs-delay`). Not yet seen inside a meeting app.

use std::ffi::c_void;
use std::path::PathBuf;

use ec_core::convert::frame_to_uyvy;
use ec_core::Frame;

use crate::SinkError;

/// The id OBS gives its camera. The same on every Mac.
const OBS_CAMERA_UID: &str = "7626645E-4425-469E-9D8B-97E0FA59AC75";

// ---------------------------------------------------------------------------
// The macOS C functions used, declared by hand. (In C#: a block of
// `[DllImport]` lines.) Names and argument orders are Apple's.
// ---------------------------------------------------------------------------

type OSStatus = i32;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CMSimpleQueueRef = *const c_void;
type CMFormatDescriptionRef = *const c_void;
type CMSampleBufferRef = *const c_void;
type CVPixelBufferRef = *const c_void;

/// "Which property of which thing": CoreMediaIO's way of asking questions.
#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

/// A time as a whole number over a scale: `value / timescale` seconds.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CMTime {
    value: i64,
    timescale: i32,
    /// 1 = "this is a real time"; 0 = "no time given".
    flags: u32,
    epoch: i64,
}

#[repr(C)]
#[derive(Default)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}

/// Something we only ever take the address of.
#[repr(C)]
struct Opaque {
    _private: [u8; 0],
}

/// Apple writes these ids as four letters packed into one number.
const fn four(code: &[u8; 4]) -> u32 {
    ((code[0] as u32) << 24) | ((code[1] as u32) << 16) | ((code[2] as u32) << 8) | code[3] as u32
}

const SYSTEM_OBJECT: u32 = 1; // kCMIOObjectSystemObject
const SCOPE_GLOBAL: u32 = four(b"glob"); // kCMIOObjectPropertyScopeGlobal
const ELEMENT_MAIN: u32 = 0; // kCMIOObjectPropertyElementMain
const PROP_DEVICES: u32 = four(b"dev#"); // kCMIOHardwarePropertyDevices
const PROP_UID: u32 = four(b"uid "); // kCMIODevicePropertyDeviceUID
const PROP_NAME: u32 = four(b"lnam"); // kCMIOObjectPropertyName
const PROP_STREAMS: u32 = four(b"stm#"); // kCMIODevicePropertyStreams
const PIXEL_UYVY: u32 = four(b"2vuy"); // kCVPixelFormatType_422YpCbCr8
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const CLOCK_UPTIME_RAW: u32 = 8;

#[link(name = "CoreMediaIO", kind = "framework")]
extern "C" {
    fn CMIOObjectGetPropertyDataSize(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> OSStatus;
    fn CMIOObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        used: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn CMIOStreamCopyBufferQueue(
        stream: u32,
        altered: extern "C" fn(u32, *mut c_void, *mut c_void),
        context: *mut c_void,
        queue: *mut CMSimpleQueueRef,
    ) -> OSStatus;
    fn CMIODeviceStartStream(device: u32, stream: u32) -> OSStatus;
    fn CMIODeviceStopStream(device: u32, stream: u32) -> OSStatus;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreate(
        allocator: *const c_void,
        codec: u32,
        width: i32,
        height: i32,
        extensions: CFDictionaryRef,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    fn CMSampleBufferCreateForImageBuffer(
        allocator: *const c_void,
        image: CVPixelBufferRef,
        data_ready: u8,
        make_ready: *const c_void,
        make_ready_context: *const c_void,
        format: CMFormatDescriptionRef,
        timing: *const CMSampleTimingInfo,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
    fn CMSampleBufferGetImageBuffer(sample: CMSampleBufferRef) -> CVPixelBufferRef;
    fn CMSimpleQueueEnqueue(queue: CMSimpleQueueRef, element: *const c_void) -> OSStatus;
    fn CMSimpleQueueDequeue(queue: CMSimpleQueueRef) -> *const c_void;
    fn CMSimpleQueueGetCount(queue: CMSimpleQueueRef) -> i32;
    fn CMSimpleQueueGetCapacity(queue: CMSimpleQueueRef) -> i32;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: usize,
        height: usize,
        pixel_format: u32,
        attributes: CFDictionaryRef,
        out: *mut CVPixelBufferRef,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CVPixelBufferRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buffer: CVPixelBufferRef) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferIsPlanar(buffer: CVPixelBufferRef) -> u8;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CVPixelBufferRef, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CVPixelBufferRef, plane: usize) -> usize;
    fn CVPixelBufferGetWidth(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(buffer: CVPixelBufferRef) -> u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: Opaque;
    static kCFTypeDictionaryValueCallBacks: Opaque;
    fn CFRelease(thing: CFTypeRef);
    fn CFStringGetCString(s: CFStringRef, buffer: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const Opaque,
        value_callbacks: *const Opaque,
    ) -> CFDictionaryRef;
}

extern "C" {
    /// Nanoseconds since the Mac started: the clock macOS stamps video with.
    fn clock_gettime_nsec_np(clock: u32) -> u64;
}

// ---------------------------------------------------------------------------
// Asking CoreMediaIO questions
// ---------------------------------------------------------------------------

fn address(selector: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    }
}

/// A property that is a list of ids (the cameras; a camera's streams).
fn id_list(object: u32, selector: u32) -> Vec<u32> {
    let addr = address(selector);
    let mut size = 0u32;
    // SAFETY: plain C calls; `size` and `ids` are ours and big enough: the
    // second call is told how many bytes `ids` holds and reports how many it used.
    unsafe {
        if CMIOObjectGetPropertyDataSize(object, &addr, 0, std::ptr::null(), &mut size) != 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / 4];
        let mut used = 0u32;
        let bytes = (ids.len() * 4) as u32;
        let data = ids.as_mut_ptr() as *mut c_void;
        if CMIOObjectGetPropertyData(object, &addr, 0, std::ptr::null(), bytes, &mut used, data)
            != 0
        {
            return Vec::new();
        }
        ids.truncate(used as usize / 4);
        ids
    }
}

/// A property that is a piece of text (a camera's name, its id).
fn text(object: u32, selector: u32) -> Option<String> {
    let addr = address(selector);
    let mut string: CFStringRef = std::ptr::null();
    let mut used = 0u32;
    let size = std::mem::size_of::<CFStringRef>() as u32;
    // SAFETY: the property is one pointer to a macOS string, which we are
    // handed ownership of and release below. The text is copied into our own
    // buffer, whose size the call is told.
    unsafe {
        let data = &mut string as *mut CFStringRef as *mut c_void;
        let status =
            CMIOObjectGetPropertyData(object, &addr, 0, std::ptr::null(), size, &mut used, data);
        if status != 0 || string.is_null() {
            return None;
        }
        let mut buffer = [0u8; 512];
        let ok = CFStringGetCString(string, buffer.as_mut_ptr(), buffer.len() as isize, UTF8);
        CFRelease(string);
        if ok == 0 {
            return None;
        }
        let end = buffer.iter().position(|b| *b == 0)?;
        String::from_utf8(buffer[..end].to_vec()).ok()
    }
}

/// One camera as macOS lists it.
#[derive(Clone, Debug)]
pub struct CameraInfo {
    pub name: String,
    /// The id that stays the same from day to day.
    pub uid: String,
    /// How many streams it has. A camera that takes frames in has two.
    pub streams: usize,
    id: u32,
}

/// Every camera macOS knows about, real and fake.
pub fn list_cameras() -> Vec<CameraInfo> {
    id_list(SYSTEM_OBJECT, PROP_DEVICES)
        .into_iter()
        .map(|id| CameraInfo {
            name: text(id, PROP_NAME).unwrap_or_default(),
            uid: text(id, PROP_UID).unwrap_or_default(),
            streams: id_list(id, PROP_STREAMS).len(),
            id,
        })
        .collect()
}

/// How far the one-time OBS set-up has got (T-072).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsState {
    /// OBS is not in an Applications folder.
    AppMissing,
    /// OBS is installed, but macOS does not list its camera: "Start Virtual
    /// Camera" was never pressed in OBS, or the camera was not allowed in
    /// System Settings.
    CameraMissing,
    /// The camera is there and can take frames.
    Ready,
}

impl ObsState {
    /// One line saying where the set-up stands.
    pub fn headline(self) -> &'static str {
        match self {
            ObsState::AppMissing => "OBS is not installed.",
            ObsState::CameraMissing => "OBS is installed, but macOS does not list its camera.",
            ObsState::Ready => "OBS's camera is installed and can take frames.",
        }
    }

    /// What the user has to do next, one step per entry; empty when nothing
    /// is left. The command-line check and the app's window both show these,
    /// so the guide is written once (T-072).
    pub fn steps(self) -> &'static [&'static str] {
        // The last three steps are the same whether OBS was just installed or
        // was already there.
        const ALLOW: &str = "If macOS asks to allow the camera: open System Settings > General > \
             Login Items & Extensions > Camera Extensions and switch OBS on. \
             Until that switch is on the camera does not exist.";
        const START: &str = "Open OBS and press \"Start Virtual Camera\" (bottom right).";
        const FINISH: &str =
            "Press \"Start Virtual Camera\" again, then \"Stop Virtual Camera\". OBS can be quit after that.";
        match self {
            ObsState::AppMissing => &[
                "Install OBS 30 or newer from https://obsproject.com and move it to Applications.",
                START,
                ALLOW,
                FINISH,
            ],
            ObsState::CameraMissing => &[START, ALLOW, FINISH],
            ObsState::Ready => &[],
        }
    }
}

fn obs_app_installed() -> bool {
    let mut places = vec![PathBuf::from("/Applications/OBS.app")];
    if let Some(home) = std::env::var_os("HOME") {
        places.push(PathBuf::from(home).join("Applications/OBS.app"));
    }
    places.iter().any(|p| p.exists())
}

fn find_obs_camera() -> Option<CameraInfo> {
    list_cameras()
        .into_iter()
        .find(|c| c.uid.eq_ignore_ascii_case(OBS_CAMERA_UID))
}

pub fn obs_state() -> ObsState {
    match find_obs_camera() {
        Some(c) if c.streams >= 2 => ObsState::Ready,
        _ if obs_app_installed() => ObsState::CameraMissing,
        _ => ObsState::AppMissing,
    }
}

// ---------------------------------------------------------------------------
// Turning a Frame into what the camera's queue takes
// ---------------------------------------------------------------------------

fn failed(what: &str, status: i32) -> SinkError {
    SinkError::Camera(format!("{what} failed (macOS error {status})"))
}

/// Makes macOS "sample buffers" (a picture plus its time) of one fixed size.
struct SampleMaker {
    width: u32,
    height: u32,
    format: CMFormatDescriptionRef,
    /// "Back this picture with shareable memory", which a buffer needs to
    /// travel to the camera's own process.
    attributes: CFDictionaryRef,
    uyvy: Vec<u8>,
}

impl SampleMaker {
    fn new(width: u32, height: u32) -> Result<SampleMaker, SinkError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) {
            return Err(SinkError::Camera(format!(
                "a {width}x{height} picture cannot be sent: the width must be even"
            )));
        }
        // SAFETY: C calls with our own out-pointers; every result is checked,
        // and what was made before a failure is released.
        unsafe {
            let mut format: CMFormatDescriptionRef = std::ptr::null();
            let status = CMVideoFormatDescriptionCreate(
                std::ptr::null(),
                PIXEL_UYVY,
                width as i32,
                height as i32,
                std::ptr::null(),
                &mut format,
            );
            if status != 0 || format.is_null() {
                return Err(failed("describing the picture format", status));
            }
            // { IOSurfaceProperties: { } }
            let empty = CFDictionaryCreate(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let keys = [kCVPixelBufferIOSurfacePropertiesKey];
            let values = [empty];
            let attributes = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            if !empty.is_null() {
                CFRelease(empty); // `attributes` keeps its own hold on it
            }
            if attributes.is_null() {
                CFRelease(format);
                return Err(failed("preparing the picture buffer settings", 0));
            }
            Ok(SampleMaker {
                width,
                height,
                format,
                attributes,
                uyvy: vec![0; width as usize * height as usize * 2],
            })
        }
    }

    /// The frame as a sample buffer stamped "now". The caller owns the result
    /// and must hand it on or release it.
    fn sample(&mut self, frame: &Frame) -> Result<CMSampleBufferRef, SinkError> {
        if (frame.width(), frame.height()) != (self.width, self.height) {
            return Err(SinkError::Camera(format!(
                "frame is {}x{}, the camera was opened for {}x{}",
                frame.width(),
                frame.height(),
                self.width,
                self.height
            )));
        }
        if !frame_to_uyvy(frame, &mut self.uyvy) {
            return Err(SinkError::Camera("frame could not be converted".into()));
        }
        let (w, h) = (self.width as usize, self.height as usize);
        // SAFETY: the buffer is created with our size and format; it is locked
        // while written; each row is written within `bytes_per_row`, which is
        // checked to hold a row; everything made is released on every path.
        unsafe {
            let mut buffer: CVPixelBufferRef = std::ptr::null();
            let status = CVPixelBufferCreate(
                std::ptr::null(),
                w,
                h,
                PIXEL_UYVY,
                self.attributes,
                &mut buffer,
            );
            if status != 0 || buffer.is_null() {
                return Err(failed("making a picture buffer", status));
            }
            let status = CVPixelBufferLockBaseAddress(buffer, 0);
            if status != 0 {
                CFRelease(buffer);
                return Err(failed("locking the picture buffer", status));
            }
            let base = CVPixelBufferGetBaseAddress(buffer);
            let bytes_per_row = CVPixelBufferGetBytesPerRow(buffer);
            let fits = !base.is_null() && bytes_per_row >= w * 2;
            if fits {
                // macOS may pad each row; copy row by row to its row length.
                for (y, row) in self.uyvy.chunks_exact(w * 2).enumerate() {
                    std::ptr::copy_nonoverlapping(row.as_ptr(), base.add(y * bytes_per_row), w * 2);
                }
            }
            CVPixelBufferUnlockBaseAddress(buffer, 0);
            if !fits {
                CFRelease(buffer);
                return Err(failed("the picture buffer has an unexpected shape", 0));
            }
            let timing = CMSampleTimingInfo {
                presentation: CMTime {
                    value: clock_gettime_nsec_np(CLOCK_UPTIME_RAW) as i64,
                    timescale: 1_000_000_000,
                    flags: 1,
                    epoch: 0,
                },
                ..Default::default()
            };
            let mut sample: CMSampleBufferRef = std::ptr::null();
            let status = CMSampleBufferCreateForImageBuffer(
                std::ptr::null(),
                buffer,
                1,
                std::ptr::null(),
                std::ptr::null(),
                self.format,
                &timing,
                &mut sample,
            );
            CFRelease(buffer); // the sample keeps its own hold on the picture
            if status != 0 || sample.is_null() {
                return Err(failed("wrapping the picture with its time", status));
            }
            Ok(sample)
        }
    }
}

impl Drop for SampleMaker {
    fn drop(&mut self) {
        // SAFETY: both were made in `new` and are released exactly once.
        unsafe {
            CFRelease(self.format);
            CFRelease(self.attributes);
        }
    }
}

// ---------------------------------------------------------------------------
// The camera itself
// ---------------------------------------------------------------------------

/// CoreMediaIO wants to be able to tell us the queue changed. We do not need to know.
extern "C" fn queue_changed(_stream: u32, _token: *mut c_void, _context: *mut c_void) {}

/// OBS's camera, opened for sending frames of one size.
pub struct ObsCamera {
    device: u32,
    stream: u32,
    queue: CMSimpleQueueRef,
    maker: SampleMaker,
    /// Frames not sent because the camera's queue was full.
    pub dropped: u64,
}

impl ObsCamera {
    /// Find OBS's camera and start its taking-in stream. Fails with a message
    /// for the user if OBS's one-time set-up is not done (see [`obs_state`]).
    pub fn open(width: u32, height: u32) -> Result<ObsCamera, SinkError> {
        let Some(camera) = find_obs_camera() else {
            return Err(SinkError::Camera(match obs_state() {
                ObsState::AppMissing => "OBS is not installed".into(),
                _ => "OBS is installed but its camera is not set up: open OBS, press \
                      \"Start Virtual Camera\" once, and allow it in System Settings"
                    .into(),
            }));
        };
        let streams = id_list(camera.id, PROP_STREAMS);
        // The first stream is the one apps read. The second takes frames in.
        // There is no name to tell them apart by, only the order.
        let Some(&stream) = streams.get(1) else {
            return Err(SinkError::Camera(
                "OBS's camera has no stream that takes frames in (OBS 30 or newer is needed)"
                    .into(),
            ));
        };
        let maker = SampleMaker::new(width, height)?;
        // SAFETY: C calls on ids macOS just gave us; results are checked, and
        // the queue is released if starting fails.
        unsafe {
            let mut queue: CMSimpleQueueRef = std::ptr::null();
            let status =
                CMIOStreamCopyBufferQueue(stream, queue_changed, std::ptr::null_mut(), &mut queue);
            if status != 0 || queue.is_null() {
                return Err(failed("getting the camera's queue", status));
            }
            let status = CMIODeviceStartStream(camera.id, stream);
            if status != 0 {
                CFRelease(queue);
                return Err(failed("starting the camera", status));
            }
            Ok(ObsCamera {
                device: camera.id,
                stream,
                queue,
                maker,
                dropped: 0,
            })
        }
    }

    /// Send one frame. `Ok(false)` means the camera's queue was full and the
    /// frame was let go: like everywhere else, we never wait.
    pub fn send(&mut self, frame: &Frame) -> Result<bool, SinkError> {
        let sample = self.maker.sample(frame)?;
        // SAFETY: `queue` is ours until drop. On success the queue's reader
        // owns the sample; otherwise we still do and release it.
        unsafe {
            let full = CMSimpleQueueGetCount(self.queue) >= CMSimpleQueueGetCapacity(self.queue);
            if full || CMSimpleQueueEnqueue(self.queue, sample) != 0 {
                CFRelease(sample);
                self.dropped += 1;
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl Drop for ObsCamera {
    fn drop(&mut self) {
        // SAFETY: stops the stream we started and releases the queue we were given.
        unsafe {
            CMIODeviceStopStream(self.device, self.stream);
            CFRelease(self.queue);
        }
    }
}

/// What [`ObsViewer`] saw in one picture from the camera.
#[derive(Clone, Debug)]
pub struct Seen {
    pub width: usize,
    pub height: usize,
    /// The pixel layout as Apple's four letters, e.g. "BGRA" or "2vuy".
    pub layout: String,
    /// The brightness (0 to 255) at each point that was asked about.
    pub brightness: Vec<u8>,
}

/// The reading end of OBS's camera, at the same low level we send at. It
/// stands in for a meeting app in tests and measurements (T-071): it reads the
/// stream apps read, by the camera's id, without touching any real camera.
/// macOS asks for camera permission for this, as it does for any reader.
pub struct ObsViewer {
    device: u32,
    stream: u32,
    queue: CMSimpleQueueRef,
}

impl ObsViewer {
    pub fn open() -> Result<ObsViewer, SinkError> {
        let camera = find_obs_camera()
            .ok_or_else(|| SinkError::Camera("OBS's camera is not there".into()))?;
        let streams = id_list(camera.id, PROP_STREAMS);
        // the first stream is the one apps read
        let Some(&stream) = streams.first() else {
            return Err(SinkError::Camera(
                "OBS's camera has no stream to read".into(),
            ));
        };
        // SAFETY: as in `ObsCamera::open`.
        unsafe {
            let mut queue: CMSimpleQueueRef = std::ptr::null();
            let status =
                CMIOStreamCopyBufferQueue(stream, queue_changed, std::ptr::null_mut(), &mut queue);
            if status != 0 || queue.is_null() {
                return Err(failed("getting the camera's reading queue", status));
            }
            let status = CMIODeviceStartStream(camera.id, stream);
            if status != 0 {
                CFRelease(queue);
                return Err(failed("starting to read the camera", status));
            }
            Ok(ObsViewer {
                device: camera.id,
                stream,
                queue,
            })
        }
    }

    /// The next picture, if one is waiting: its size, layout, and how bright it
    /// is at each of `points` (positions from 0 to 1 across and down). Never waits.
    pub fn look(&mut self, points: &[(f32, f32)]) -> Option<Seen> {
        // SAFETY: what comes off the queue is a sample buffer we now own and
        // release below. Its picture is locked while read, and every position
        // read is inside the row and plane sizes the buffer itself reports.
        unsafe {
            let sample = CMSimpleQueueDequeue(self.queue);
            if sample.is_null() {
                return None;
            }
            let buffer = CMSampleBufferGetImageBuffer(sample);
            if buffer.is_null() || CVPixelBufferLockBaseAddress(buffer, 1) != 0 {
                CFRelease(sample);
                return None;
            }
            let (width, height) = (
                CVPixelBufferGetWidth(buffer),
                CVPixelBufferGetHeight(buffer),
            );
            let code = CVPixelBufferGetPixelFormatType(buffer);
            let layout = String::from_utf8_lossy(&code.to_be_bytes()).into_owned();
            // Where the brightness of pixel x sits within its row:
            // (bytes per pixel, offset of the brightness byte).
            let planar = CVPixelBufferIsPlanar(buffer) != 0;
            let (base, row_len, step, offset) = if planar {
                // two-plane layouts (NV12): the first plane is brightness alone
                (
                    CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                    CVPixelBufferGetBytesPerRowOfPlane(buffer, 0),
                    1,
                    0,
                )
            } else if code == PIXEL_UYVY {
                (
                    CVPixelBufferGetBaseAddress(buffer),
                    CVPixelBufferGetBytesPerRow(buffer),
                    2,
                    1,
                )
            } else {
                // BGRA and its relatives: take green, which carries most of the brightness
                (
                    CVPixelBufferGetBaseAddress(buffer),
                    CVPixelBufferGetBytesPerRow(buffer),
                    4,
                    1,
                )
            };
            let mut brightness = Vec::with_capacity(points.len());
            if !base.is_null() && width > 0 && height > 0 && row_len >= width * step {
                for (u, v) in points {
                    let x = ((u.clamp(0.0, 1.0) * width as f32) as usize).min(width - 1);
                    let y = ((v.clamp(0.0, 1.0) * height as f32) as usize).min(height - 1);
                    brightness.push(*base.add(y * row_len + x * step + offset));
                }
            }
            CVPixelBufferUnlockBaseAddress(buffer, 1);
            CFRelease(sample);
            Some(Seen {
                width,
                height,
                layout,
                brightness,
            })
        }
    }
}

impl Drop for ObsViewer {
    fn drop(&mut self) {
        // SAFETY: stops the stream we started and releases the queue we were given.
        unsafe {
            CMIODeviceStopStream(self.device, self.stream);
            CFRelease(self.queue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::PixelFormat;

    #[test]
    fn the_guide_has_steps_exactly_when_something_is_left_to_do() {
        assert!(ObsState::Ready.steps().is_empty());
        assert_eq!(ObsState::CameraMissing.steps().len(), 3);
        // a missing app needs the same steps, with "install" in front
        assert_eq!(
            ObsState::AppMissing.steps()[1..],
            *ObsState::CameraMissing.steps()
        );
    }

    #[test]
    fn four_letter_ids_pack_the_way_apple_does() {
        assert_eq!(four(b"glob"), 0x676c_6f62);
        assert_eq!(four(b"2vuy"), 0x3276_7579);
    }

    #[test]
    fn the_timing_block_has_the_size_macos_expects() {
        assert_eq!(std::mem::size_of::<CMTime>(), 24);
        assert_eq!(std::mem::size_of::<CMSampleTimingInfo>(), 72);
    }

    /// Runs on any Mac, with or without a camera or OBS: the answer must only be sane.
    #[test]
    fn listing_cameras_and_the_obs_check_agree() {
        let cameras = list_cameras();
        for c in &cameras {
            assert!(!c.uid.is_empty(), "{c:?}");
        }
        let has_obs = cameras
            .iter()
            .any(|c| c.uid.eq_ignore_ascii_case(OBS_CAMERA_UID) && c.streams >= 2);
        assert_eq!(obs_state() == ObsState::Ready, has_obs);
    }

    /// Everything up to the camera's queue: the frame becomes a macOS sample
    /// buffer holding the right picture.
    #[test]
    fn a_frame_becomes_a_sample_buffer_with_the_same_picture() {
        let (w, h) = (64u32, 36u32);
        let yuyv: Vec<u8> = (0..w * h * 2).map(|i| (i * 7 % 251) as u8).collect();
        let frame = Frame::new(w, h, PixelFormat::Yuyv, yuyv.clone(), 0).unwrap();
        let mut maker = SampleMaker::new(w, h).unwrap();
        for _ in 0..3 {
            let sample = maker.sample(&frame).unwrap();
            // SAFETY: reading back a buffer we just made, locked while read.
            unsafe {
                let buffer = CMSampleBufferGetImageBuffer(sample);
                assert!(!buffer.is_null());
                assert_eq!(CVPixelBufferGetWidth(buffer), w as usize);
                assert_eq!(CVPixelBufferGetHeight(buffer), h as usize);
                assert_eq!(CVPixelBufferGetPixelFormatType(buffer), PIXEL_UYVY);
                assert_eq!(CVPixelBufferLockBaseAddress(buffer, 1), 0); // 1 = read only
                let base = CVPixelBufferGetBaseAddress(buffer);
                let stride = CVPixelBufferGetBytesPerRow(buffer);
                for y in 0..h as usize {
                    let row = std::slice::from_raw_parts(base.add(y * stride), w as usize * 2);
                    // UYVY is YUYV with each pair of bytes swapped
                    let sent = &yuyv[y * w as usize * 2..][..w as usize * 2];
                    let (got, want) = (row.as_chunks::<2>().0, sent.as_chunks::<2>().0);
                    for (got, want) in got.iter().zip(want) {
                        assert_eq!([got[1], got[0]], *want);
                    }
                }
                CVPixelBufferUnlockBaseAddress(buffer, 1);
                CFRelease(sample);
            }
        }
    }

    #[test]
    fn a_frame_of_another_size_or_an_odd_width_is_refused() {
        assert!(SampleMaker::new(63, 36).is_err());
        let mut maker = SampleMaker::new(64, 36).unwrap();
        let other = Frame::new(32, 36, PixelFormat::Yuyv, vec![0; 32 * 36 * 2], 0).unwrap();
        assert!(maker.sample(&other).is_err());
    }
}
