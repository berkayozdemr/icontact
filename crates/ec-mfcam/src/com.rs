//! The objects Windows talks to. Windows only.
//!
//! Windows' camera system (Media Foundation) wants a camera to be a set of
//! COM objects. COM is the old Windows way for programs to hand objects to
//! each other; in .NET you meet it as "COM interop". Each object promises
//! some interfaces (lists of methods), and Windows calls those methods.
//!
//! Who creates what, when an app opens "Icontact Camera":
//!
//! 1. Windows loads this DLL and calls [`DllGetClassObject`]. It gets a
//!    [`Factory`].
//! 2. It asks the factory for an object. It gets an [`Activate`]: a small
//!    "I can make the camera" object with a bag of settings.
//! 3. It tells the Activate to make the real thing. It gets a [`Source`]:
//!    the camera. The source owns one [`Stream`]: the video.
//! 4. Windows starts the source and then calls `RequestSample` on the stream
//!    for every picture. The stream answers with an event carrying the picture.
//!
//! Almost everything here follows Microsoft's "SimpleMediaSource" sample and
//! the open source VCamSample project; the comments say where a value was
//! taken from them rather than understood.
//!
//! Written on a Mac; reported working on one Windows 11 PC (2026-11-16).

#![allow(non_snake_case)] // the method names are Windows' own

use std::ffi::c_void;
use std::sync::Mutex;

use windows::core::{
    implement, ComObject, IUnknown, Interface, Ref, BOOL, GUID, HRESULT, PCWSTR, PWSTR,
};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, ERROR_SET_NOT_FOUND, E_POINTER, S_FALSE, S_OK,
};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSCAMERAPROFILE_Legacy, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};

use crate::feed::{Feed, Showing, FPS, HEIGHT, PICTURE_LEN, WIDTH};
use crate::log;

/// The camera's class id: the name Windows knows this DLL's camera by. The
/// same value, as text, is `ec_sink::WIN_CAMERA_CLSID`; the install script
/// writes it into the registry and the app passes it to Windows.
const CLSID_ICONTACT_CAMERA: GUID = GUID::from_u128(0x3233B581_9067_4B31_A3F2_C1C07D73BA7E);

/// "No such property set": the answer to every hardware question (zoom,
/// focus, ...) a meeting app may ask through `IKsControl`.
fn not_found() -> windows::core::Error {
    windows::core::Error::from(ERROR_SET_NOT_FOUND)
}

/// Lock a mutex even if another thread panicked while holding it. Inside
/// Windows' camera service a second panic would take the service down.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Stream: the video
// ---------------------------------------------------------------------------

#[implement(IMFMediaStream2, IKsControl)]
struct Stream {
    inner: Mutex<StreamInner>,
}

struct StreamInner {
    /// Windows collects our answers (events) from this queue.
    queue: IMFMediaEventQueue,
    /// Facts about the stream that Windows reads once.
    attributes: IMFAttributes,
    /// The list of picture formats on offer (just one).
    descriptor: IMFStreamDescriptor,
    /// The source this stream belongs to. Cleared at shutdown: the source
    /// holds the stream and the stream holds the source, and such a ring
    /// would otherwise never be freed.
    source: Option<IMFMediaSource>,
    state: MF_STREAM_STATE,
    feed: Feed,
    /// What went out last, to log only the changes.
    showing: Option<Showing>,
}

impl Stream {
    fn new() -> windows::core::Result<Stream> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            let attributes = attributes.ok_or(E_POINTER)?;
            // "This is a video capture pin", stream number 0, shareable
            // between apps, and it carries a colour picture. (All four as in
            // Microsoft's sample.)
            attributes.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            attributes.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            attributes.SetUINT32(
                &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
                MFFrameSourceTypes_Color.0 as u32,
            )?;

            // The one picture format: 1280x720 NV12, 30 a second.
            let kind = MFCreateMediaType()?;
            kind.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            kind.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            kind.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            kind.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            // A size or a ratio is two 32-bit numbers packed into one 64-bit value.
            let pair = |a: u32, b: u32| ((a as u64) << 32) | b as u64;
            kind.SetUINT64(&MF_MT_FRAME_SIZE, pair(WIDTH, HEIGHT))?;
            kind.SetUINT64(&MF_MT_FRAME_RATE, pair(FPS, 1))?;
            kind.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))?;
            // Bytes per row of brightness: no padding, so the width.
            kind.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH)?;
            kind.SetUINT32(&MF_MT_AVG_BITRATE, PICTURE_LEN as u32 * 8 * FPS)?;

            let descriptor = MFCreateStreamDescriptor(0, &[Some(kind.clone())])?;
            descriptor
                .GetMediaTypeHandler()?
                .SetCurrentMediaType(&kind)?;

            Ok(Stream {
                inner: Mutex::new(StreamInner {
                    queue: MFCreateEventQueue()?,
                    attributes,
                    descriptor,
                    source: None,
                    state: MF_STREAM_STATE_STOPPED,
                    feed: Feed::new(ec_sink::default_path()),
                    showing: None,
                }),
            })
        }
    }

    fn set_source(&self, source: Option<IMFMediaSource>) {
        lock(&self.inner).source = source;
    }

    fn attributes(&self) -> IMFAttributes {
        lock(&self.inner).attributes.clone()
    }

    fn descriptor(&self) -> IMFStreamDescriptor {
        lock(&self.inner).descriptor.clone()
    }

    fn start(&self) -> windows::core::Result<()> {
        let mut s = lock(&self.inner);
        s.state = MF_STREAM_STATE_RUNNING;
        unsafe {
            s.queue.QueueEventParamVar(
                MEStreamStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn stop(&self) -> windows::core::Result<()> {
        let mut s = lock(&self.inner);
        s.state = MF_STREAM_STATE_STOPPED;
        s.feed.close();
        unsafe {
            s.queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn shutdown(&self) {
        let mut s = lock(&self.inner);
        s.state = MF_STREAM_STATE_STOPPED;
        s.feed.close();
        s.source = None;
        unsafe {
            let _ = s.queue.Shutdown();
        }
    }
}

/// One NV12 picture wrapped the way Windows passes video around: bytes in a
/// buffer, the buffer in a "sample" with a time stamp.
fn make_sample(picture: &[u8]) -> windows::core::Result<IMFSample> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(picture.len() as u32)?;
        let mut at = std::ptr::null_mut();
        buffer.Lock(&mut at, None, None)?;
        if !at.is_null() {
            std::ptr::copy_nonoverlapping(picture.as_ptr(), at, picture.len());
        }
        buffer.Unlock()?;
        buffer.SetCurrentLength(picture.len() as u32)?;

        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        // Times are in units of 100 nanoseconds.
        sample.SetSampleTime(MFGetSystemTime())?;
        sample.SetSampleDuration(10_000_000 / FPS as i64)?;
        Ok(sample)
    }
}

impl IMFMediaEventGenerator_Impl for Stream_Impl {
    // The four event methods only pass the call on to the queue.
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        // Take the queue out first: GetEvent may wait, and must not wait
        // while holding our lock.
        let queue = lock(&self.inner).queue.clone();
        unsafe { queue.GetEvent(flags.0) }
    }

    fn BeginGetEvent(
        &self,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        let queue = lock(&self.inner).queue.clone();
        unsafe { queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        let queue = lock(&self.inner).queue.clone();
        unsafe { queue.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        let queue = lock(&self.inner).queue.clone();
        unsafe { queue.QueueEventParamVar(met, extended, status, value) }
    }
}

impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        lock(&self.inner).source.clone().ok_or(MF_E_SHUTDOWN.into())
    }

    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        Ok(lock(&self.inner).descriptor.clone())
    }

    /// "Give me the next picture." Called by Windows for every frame.
    /// `token` is a ticket Windows wants back with the picture.
    fn RequestSample(&self, token: Ref<IUnknown>) -> windows::core::Result<()> {
        let mut s = lock(&self.inner);
        if s.state != MF_STREAM_STATE_RUNNING {
            return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());
        }
        // Waits about one frame time; this is what sets the pace.
        let (picture, showing) = s.feed.next_picture();
        let sample = make_sample(picture)?;
        if s.showing != Some(showing) {
            s.showing = Some(showing);
            log::line(&format!("stream: now sending {showing:?}"));
        }
        unsafe {
            if let Some(token) = token.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            s.queue
                .QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)
        }
    }
}

impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows::core::Result<()> {
        if value == MF_STREAM_STATE_RUNNING {
            self.start()
        } else if value == MF_STREAM_STATE_STOPPED {
            self.stop()
        } else {
            Err(MF_E_INVALID_STATE_TRANSITION.into()) // no pausing
        }
    }

    fn GetStreamState(&self) -> windows::core::Result<MF_STREAM_STATE> {
        Ok(lock(&self.inner).state)
    }
}

impl IKsControl_Impl for Stream_Impl {
    fn KsProperty(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
    fn KsMethod(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
    fn KsEvent(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
}

// ---------------------------------------------------------------------------
// Source: the camera
// ---------------------------------------------------------------------------

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
struct Source {
    queue: IMFMediaEventQueue,
    attributes: IMFAttributes,
    /// The list of streams (one) that Windows picks from.
    descriptor: IMFPresentationDescriptor,
    stream: ComObject<Stream>,
    shut: Mutex<bool>,
}

impl Source {
    fn new() -> windows::core::Result<Source> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let attributes = attributes.ok_or(E_POINTER)?;

            // A "sensor profile": Windows asks which combinations of size and
            // speed the camera supports. One plain profile for stream 0, up to
            // 30 a second. (Text and values taken from Microsoft's sample.)
            let profiles = MFCreateSensorProfileCollection()?;
            let profile = MFCreateSensorProfile(&KSCAMERAPROFILE_Legacy, 0, PCWSTR::null())?;
            profile.AddProfileFilter(0, windows::core::w!("((RES==;FRT<=30,1;SUT==))"))?;
            profiles.AddProfile(&profile)?;
            attributes.SetUnknown(&MF_DEVICEMFT_SENSORPROFILE_COLLECTION, &profiles)?;

            let stream = ComObject::new(Stream::new()?);
            let descriptor = MFCreatePresentationDescriptor(Some(&[Some(stream.descriptor())]))?;
            descriptor.SelectStream(0)?;

            Ok(Source {
                queue: MFCreateEventQueue()?,
                attributes,
                descriptor,
                stream,
                shut: Mutex::new(false),
            })
        }
    }

    fn check_open(&self) -> windows::core::Result<()> {
        if *lock(&self.shut) {
            Err(MF_E_SHUTDOWN.into())
        } else {
            Ok(())
        }
    }
}

/// "Now", as the value Windows expects with started and stopped events.
fn now_value() -> PROPVARIANT {
    PROPVARIANT::from(unsafe { MFGetSystemTime() })
}

impl IMFMediaEventGenerator_Impl for Source_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows::core::Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }

    fn BeginGetEvent(
        &self,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        unsafe { self.queue.QueueEventParamVar(met, extended, status, value) }
    }
}

impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        self.check_open()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32) // a live camera, not a file
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        self.check_open()?;
        unsafe { self.descriptor.Clone() }
    }

    /// Windows starts the camera. It passes back the stream list with the
    /// streams it wants ticked.
    fn Start(
        &self,
        descriptor: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        _position: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        self.check_open()?;
        // Only "no special time format" is supported.
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        log::line("source: Start");
        unsafe {
            let wanted = match descriptor.as_ref() {
                Some(d) => {
                    let mut selected = BOOL(0);
                    let mut found = None;
                    d.GetStreamDescriptorByIndex(0, &mut selected, &mut found)?;
                    selected.as_bool()
                }
                None => true,
            };
            if wanted {
                // "Here is the stream" first, then start it, then say that
                // the source has started. (Order as in Microsoft's sample.)
                // (ask for the newest interface we implement, then for the plain one)
                let stream: IMFMediaStream =
                    self.stream.to_interface::<IMFMediaStream2>().cast()?;
                self.queue.QueueEventParamUnk(
                    MENewStream.0 as u32,
                    &GUID::zeroed(),
                    S_OK,
                    &stream,
                )?;
                self.stream.start()?;
            }
            self.queue.QueueEventParamVar(
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &now_value(),
            )
        }
    }

    fn Stop(&self) -> windows::core::Result<()> {
        self.check_open()?;
        log::line("source: Stop");
        self.stream.stop()?;
        unsafe {
            self.queue.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &now_value(),
            )
        }
    }

    fn Pause(&self) -> windows::core::Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into()) // a live camera cannot pause
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        log::line("source: Shutdown");
        *lock(&self.shut) = true;
        self.stream.shutdown();
        unsafe { self.queue.Shutdown() }
    }
}

impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, stream: u32) -> windows::core::Result<IMFAttributes> {
        if stream == 0 {
            Ok(self.stream.attributes())
        } else {
            Err(MF_E_INVALIDSTREAMNUMBER.into())
        }
    }

    /// Windows offers its graphics device, for cameras that make their
    /// pictures on the graphics card. Ours are plain memory: not needed.
    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> windows::core::Result<()> {
        Ok(())
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _riid: *const GUID,
        _out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into()) // no extra services
    }
}

impl IKsControl_Impl for Source_Impl {
    fn KsProperty(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
    fn KsMethod(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
    fn KsEvent(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut c_void,
        _: u32,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        Err(not_found())
    }
}

// ---------------------------------------------------------------------------
// Activate: "I can make the camera"
// ---------------------------------------------------------------------------

/// Windows wants this object to also be a bag of settings (`IMFAttributes`,
/// about thirty methods). The bag itself is one Windows makes for us; every
/// method below `ActivateObject` just passes the call on to it.
#[implement(IMFActivate)]
struct Activate {
    bag: IMFAttributes,
    source: Mutex<Option<ComObject<Source>>>,
}

impl Activate {
    fn new() -> windows::core::Result<Activate> {
        unsafe {
            let mut bag = None;
            MFCreateAttributes(&mut bag, 2)?;
            let bag = bag.ok_or(E_POINTER)?;
            // Both as in VCamSample: let Windows pair us with a real camera's
            // extras if it wants, and say which class made this object.
            bag.SetUINT32(&MF_VIRTUALCAMERA_PROVIDE_ASSOCIATED_CAMERA_SOURCES, 1)?;
            bag.SetGUID(&MFT_TRANSFORM_CLSID_Attribute, &CLSID_ICONTACT_CAMERA)?;
            Ok(Activate {
                bag,
                source: Mutex::new(None),
            })
        }
    }
}

impl IMFActivate_Impl for Activate_Impl {
    /// "Make the camera and give me this interface of it."
    fn ActivateObject(
        &self,
        riid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if riid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        let mut slot = lock(&self.source);
        if slot.is_none() {
            log::line("activate: making the camera source");
            let source = ComObject::new(Source::new().inspect_err(|e| {
                log::line(&format!("activate: making the source failed: {e}"));
            })?);
            // tell the stream who its source is
            let as_source: IMFMediaSource = source.to_interface::<IMFMediaSourceEx>().cast()?;
            source.stream.set_source(Some(as_source));
            *slot = Some(source);
        }
        let source: IMFMediaSourceEx = match slot.as_ref() {
            Some(s) => s.to_interface(),
            None => return Err(E_POINTER.into()),
        };
        unsafe { source.query(riid, out).ok() }
    }

    fn ShutdownObject(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn DetachObject(&self) -> windows::core::Result<()> {
        *lock(&self.source) = None;
        Ok(())
    }
}

// The settings bag: every method passes the call straight on.
impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.bag.GetItem(key, (!value.is_null()).then_some(value)) }
    }
    fn GetItemType(&self, key: *const GUID) -> windows::core::Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.bag.GetItemType(key) }
    }
    fn CompareItem(
        &self,
        key: *const GUID,
        value: *const PROPVARIANT,
    ) -> windows::core::Result<BOOL> {
        unsafe { self.bag.CompareItem(key, value) }
    }
    fn Compare(
        &self,
        theirs: Ref<IMFAttributes>,
        how: MF_ATTRIBUTES_MATCH_TYPE,
    ) -> windows::core::Result<BOOL> {
        unsafe { self.bag.Compare(theirs.as_ref(), how) }
    }
    fn GetUINT32(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.bag.GetUINT32(key) }
    }
    fn GetUINT64(&self, key: *const GUID) -> windows::core::Result<u64> {
        unsafe { self.bag.GetUINT64(key) }
    }
    fn GetDouble(&self, key: *const GUID) -> windows::core::Result<f64> {
        unsafe { self.bag.GetDouble(key) }
    }
    fn GetGUID(&self, key: *const GUID) -> windows::core::Result<GUID> {
        unsafe { self.bag.GetGUID(key) }
    }
    fn GetStringLength(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.bag.GetStringLength(key) }
    }
    fn GetString(
        &self,
        key: *const GUID,
        value: PWSTR,
        size: u32,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        if value.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            let buffer = std::slice::from_raw_parts_mut(value.0, size as usize);
            self.bag
                .GetString(key, buffer, (!length.is_null()).then_some(length))
        }
    }
    fn GetAllocatedString(
        &self,
        key: *const GUID,
        value: *mut PWSTR,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe { self.bag.GetAllocatedString(key, value, length) }
    }
    fn GetBlobSize(&self, key: *const GUID) -> windows::core::Result<u32> {
        unsafe { self.bag.GetBlobSize(key) }
    }
    fn GetBlob(
        &self,
        key: *const GUID,
        buf: *mut u8,
        size: u32,
        got: *mut u32,
    ) -> windows::core::Result<()> {
        if buf.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            let buffer = std::slice::from_raw_parts_mut(buf, size as usize);
            self.bag
                .GetBlob(key, buffer, (!got.is_null()).then_some(got))
        }
    }
    fn GetAllocatedBlob(
        &self,
        key: *const GUID,
        buf: *mut *mut u8,
        size: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe { self.bag.GetAllocatedBlob(key, buf, size) }
    }
    fn GetUnknown(
        &self,
        key: *const GUID,
        riid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if riid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            let unknown: IUnknown = self.bag.GetUnknown(key)?;
            unknown.query(riid, out).ok()
        }
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<()> {
        unsafe { self.bag.SetItem(key, value) }
    }
    fn DeleteItem(&self, key: *const GUID) -> windows::core::Result<()> {
        unsafe { self.bag.DeleteItem(key) }
    }
    fn DeleteAllItems(&self) -> windows::core::Result<()> {
        unsafe { self.bag.DeleteAllItems() }
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> windows::core::Result<()> {
        unsafe { self.bag.SetUINT32(key, value) }
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> windows::core::Result<()> {
        unsafe { self.bag.SetUINT64(key, value) }
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> windows::core::Result<()> {
        unsafe { self.bag.SetDouble(key, value) }
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> windows::core::Result<()> {
        unsafe { self.bag.SetGUID(key, value) }
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> windows::core::Result<()> {
        unsafe { self.bag.SetString(key, *value) }
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> windows::core::Result<()> {
        if buf.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            self.bag
                .SetBlob(key, std::slice::from_raw_parts(buf, size as usize))
        }
    }
    fn SetUnknown(&self, key: *const GUID, unknown: Ref<IUnknown>) -> windows::core::Result<()> {
        unsafe { self.bag.SetUnknown(key, unknown.as_ref()) }
    }
    fn LockStore(&self) -> windows::core::Result<()> {
        unsafe { self.bag.LockStore() }
    }
    fn UnlockStore(&self) -> windows::core::Result<()> {
        unsafe { self.bag.UnlockStore() }
    }
    fn GetCount(&self) -> windows::core::Result<u32> {
        unsafe { self.bag.GetCount() }
    }
    fn GetItemByIndex(
        &self,
        index: u32,
        key: *mut GUID,
        value: *mut PROPVARIANT,
    ) -> windows::core::Result<()> {
        unsafe {
            self.bag
                .GetItemByIndex(index, key, (!value.is_null()).then_some(value))
        }
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> windows::core::Result<()> {
        unsafe { self.bag.CopyAllItems(dest.as_ref()) }
    }
}

// ---------------------------------------------------------------------------
// Factory and the functions Windows calls by name
// ---------------------------------------------------------------------------

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if riid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into()); // an old COM feature we do not offer
        }
        let activate: IMFActivate = Activate::new()
            .inspect_err(|e| log::line(&format!("factory: making Activate failed: {e}")))?
            .into();
        unsafe { activate.query(riid, out).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The first call Windows makes after loading the DLL: "give me the factory
/// for this class id". `extern "system"` is the Windows calling convention;
/// `#[no_mangle]` keeps the name exactly as written so Windows can find it.
///
/// # Safety
/// Called by Windows with valid pointers, as COM promises.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = std::ptr::null_mut();
        if *clsid != CLSID_ICONTACT_CAMERA {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        log::line("DllGetClassObject: the camera was asked for");
        let factory: IClassFactory = Factory.into();
        factory.query(riid, out)
    }
}

/// "May I unload you?" Always "not now": the camera service loads us once
/// and keeps us, and saying no is the safe answer while objects may be alive.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}
