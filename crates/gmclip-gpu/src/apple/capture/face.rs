//! The face file: the camera at its current mode and the mic as the file's second
//! track (decision 0045: no separate WAV), from one `AVCaptureSession`, written by
//! [`Writer`]. Like the screen, it runs warm until [`FaceRecorder::go`] gives it `t0`.
//! The tracks are set up from the first camera frame and the first mic buffer after
//! that (their size and format are only known then), so the file's first frames sit a
//! few milliseconds after `t0`, on the same clock as the screen.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::sources::av_device;
use super::writer::{VideoSettings, Writer, WriterStats};
use super::{
    Asbd, CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBufferDataIsReady,
    CMSampleBufferGetFormatDescription, CMSampleBufferGetImageBuffer, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CaptureError, Registry, class, error_text, ns_dict, ns_number_i64,
    sample_host_time, serial_queue,
};

unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const AnyObject;
}

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` ('420v').
const PIXEL_420V: i64 = 0x3432_3076;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceConfig {
    pub path: PathBuf,
    /// A camera's unique id (the default camera when absent).
    pub camera: Option<String>,
    /// A mic's unique id (the default mic when absent).
    pub mic: Option<String>,
    /// Record the mic at all.
    pub with_mic: bool,
    pub hevc: bool,
}

struct Ctx {
    writer: Mutex<Option<Writer>>,
    t0: AtomicU64,
    live: AtomicBool,
    video_output: AtomicUsize,
    with_mic: bool,
    hevc: bool,
    /// The camera's frame size and the mic's format, once seen.
    video: Mutex<Option<(u32, u32)>>,
    audio: Mutex<Option<(f64, u32)>>,
    failed: Mutex<Option<String>>,
}

fn registry() -> &'static Registry<Ctx> {
    static R: Registry<Ctx> = Registry::new();
    &R
}

impl Ctx {
    /// Adds the tracks and starts the file once both formats are known.
    fn ready(&self, w: &mut Writer) {
        if w.started() {
            return;
        }
        let t0 = self.t0.load(Ordering::Acquire);
        if t0 == 0 {
            return;
        }
        let v = self.video.lock().ok().and_then(|g| *g);
        let a = self.audio.lock().ok().and_then(|g| *g);
        let Some((vw, vh)) = v else { return };
        if self.with_mic && a.is_none() {
            return;
        }
        let r = (|| {
            if !w.has_video() {
                w.add_video(VideoSettings {
                    w: vw,
                    h: vh,
                    fps: 30,
                    hevc: self.hevc,
                })?;
            }
            if let (Some((rate, ch)), false) = (a, w.has_audio()) {
                w.add_audio(rate, ch)?;
            }
            w.start(t0)
        })();
        if let Err(e) = r
            && let Ok(mut f) = self.failed.lock()
        {
            *f = Some(e.to_string());
        }
    }
}

extern "C-unwind" fn did_output(
    this: &AnyObject,
    _: Sel,
    output: *mut AnyObject,
    sbuf: *mut c_void,
    _conn: *mut AnyObject,
) {
    let Some(ctx) = registry().get(this as *const AnyObject as usize) else {
        return;
    };
    if sbuf.is_null() || !ctx.live.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: a live sample buffer handed to this callback.
    if unsafe { CMSampleBufferDataIsReady(sbuf) } == 0 {
        return;
    }
    let video = output as usize == ctx.video_output.load(Ordering::Acquire);
    if video {
        // SAFETY: a camera frame carries a pixel buffer (or NULL, checked).
        let pb = unsafe { CMSampleBufferGetImageBuffer(sbuf) };
        if pb.is_null() {
            return;
        }
        if let Ok(mut g) = ctx.video.lock()
            && g.is_none()
        {
            // SAFETY: a live pixel buffer.
            let (w, h) = unsafe { (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb)) };
            *g = Some((w as u32, h as u32));
        }
    } else if let Ok(mut g) = ctx.audio.lock()
        && g.is_none()
    {
        // SAFETY: an audio buffer's format description and its ASBD (or NULL).
        let asbd: *const Asbd = unsafe {
            let desc = CMSampleBufferGetFormatDescription(sbuf);
            if desc.is_null() {
                std::ptr::null()
            } else {
                CMAudioFormatDescriptionGetStreamBasicDescription(desc)
            }
        };
        if !asbd.is_null() {
            // SAFETY: non-null, owned by the format description for the callback.
            let a = unsafe { *asbd };
            *g = Some((a.sample_rate, a.channels_per_frame));
        }
    }
    if let Ok(mut g) = ctx.writer.lock()
        && let Some(w) = g.as_mut()
    {
        ctx.ready(w);
        if w.started() && sample_host_time(sbuf) >= ctx.t0.load(Ordering::Acquire) {
            w.append(video, sbuf);
        }
    }
}

fn delegate_class() -> Result<&'static AnyClass, CaptureError> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            let superclass = AnyClass::get(c"NSObject")?;
            let mut b = ClassBuilder::new(c"GmclipFaceOutput", superclass)?;
            for p in [
                c"AVCaptureVideoDataOutputSampleBufferDelegate",
                c"AVCaptureAudioDataOutputSampleBufferDelegate",
            ] {
                if let Some(p) = AnyProtocol::get(p) {
                    b.add_protocol(p);
                }
            }
            // SAFETY: matches -captureOutput:didOutputSampleBuffer:fromConnection: (id,
            // SEL, AVCaptureOutput *, CMSampleBufferRef, AVCaptureConnection *).
            unsafe {
                b.add_method(
                    sel!(captureOutput:didOutputSampleBuffer:fromConnection:),
                    did_output as extern "C-unwind" fn(_, _, _, _, _),
                );
            }
            Some(b.register())
        })
        .ok_or(CaptureError::Missing(
            "AVCaptureVideoDataOutputSampleBufferDelegate",
        ))
}

/// A running camera + mic capture.
pub struct FaceRecorder {
    session: Retained<AnyObject>,
    delegate: Retained<AnyObject>,
    _queue: Retained<AnyObject>,
    ctx: Arc<Ctx>,
    camera: String,
    mic: Option<String>,
}

impl std::fmt::Debug for FaceRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceRecorder")
            .field("camera", &self.camera)
            .field("mic", &self.mic)
            .finish_non_exhaustive()
    }
}

// SAFETY: the session and delegate are only messaged from the owning thread; the
// capture callbacks reach shared state through `Ctx`'s atomics and mutexes.
unsafe impl Send for FaceRecorder {}

pub(super) fn device_input(device: &AnyObject) -> Result<Retained<AnyObject>, CaptureError> {
    let c = class("AVCaptureDeviceInput")?;
    let mut err: *mut AnyObject = std::ptr::null_mut();
    // SAFETY: a live capture device and an NSError out-pointer.
    let i: Option<Retained<AnyObject>> =
        unsafe { msg_send![c, deviceInputWithDevice: device, error: &mut err] };
    i.ok_or_else(|| CaptureError::Os {
        step: "AVCaptureDeviceInput",
        message: error_text(err),
    })
}

fn name_of(device: &AnyObject) -> String {
    // SAFETY: -localizedName is an NSString.
    let n: Option<Retained<AnyObject>> = unsafe { msg_send![device, localizedName] };
    super::rust_string(n.as_deref()).unwrap_or_default()
}

pub(super) fn add(
    session: &AnyObject,
    input: bool,
    obj: &AnyObject,
    what: &'static str,
) -> Result<(), CaptureError> {
    // SAFETY: -canAddInput:/-addInput: or -canAddOutput:/-addOutput: on a session
    // inside begin/commitConfiguration.
    unsafe {
        let ok: Bool = if input {
            msg_send![session, canAddInput: obj]
        } else {
            msg_send![session, canAddOutput: obj]
        };
        if !ok.as_bool() {
            return Err(CaptureError::Os {
                step: what,
                message: "the capture session cannot take it".into(),
            });
        }
        if input {
            let () = msg_send![session, addInput: obj];
        } else {
            let () = msg_send![session, addOutput: obj];
        }
    }
    Ok(())
}

impl FaceRecorder {
    /// Starts the camera (and mic); nothing is written until [`Self::go`].
    pub fn start(cfg: &FaceConfig) -> Result<Self, CaptureError> {
        let perms = super::permissions();
        if !perms.camera.granted() {
            return Err(CaptureError::Permission("Camera"));
        }
        let with_mic = cfg.with_mic && perms.microphone.granted();
        let cam = av_device(true, cfg.camera.as_deref())?;
        let mic = if with_mic {
            Some(av_device(false, cfg.mic.as_deref())?)
        } else {
            None
        };

        let sc = class("AVCaptureSession")?;
        // SAFETY: +new.
        let session: Option<Retained<AnyObject>> = unsafe { msg_send![sc, new] };
        let session = session.ok_or(CaptureError::Missing("AVCaptureSession"))?;
        let dc = delegate_class()?;
        // SAFETY: +new on the NSObject subclass registered above.
        let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![dc, new] };
        let delegate = delegate.ok_or(CaptureError::Missing("GmclipFaceOutput"))?;
        let queue = serial_queue(c"net.gmclip.capture.face")?;

        // SAFETY: configuration brackets on a new session.
        let () = unsafe { msg_send![&*session, beginConfiguration] };
        add(&session, true, &*device_input(&cam)?, "camera input")?;
        if let Some(m) = &mic {
            add(&session, true, &*device_input(m)?, "mic input")?;
        }
        let vc = class("AVCaptureVideoDataOutput")?;
        // SAFETY: +new.
        let vout: Option<Retained<AnyObject>> = unsafe { msg_send![vc, new] };
        let vout = vout.ok_or(CaptureError::Missing("AVCaptureVideoDataOutput"))?;
        // SAFETY: a CoreVideo constant key; plain setters; the output keeps the delegate
        // weakly (held by the recorder) and the queue strongly.
        unsafe {
            let fmt = ns_number_i64(PIXEL_420V)?;
            let settings = ns_dict(&[(&*kCVPixelBufferPixelFormatTypeKey, &*fmt)])?;
            let () = msg_send![&*vout, setVideoSettings: &*settings];
            let () = msg_send![&*vout, setAlwaysDiscardsLateVideoFrames: Bool::YES];
            let () = msg_send![&*vout, setSampleBufferDelegate: &*delegate, queue: &*queue];
        }
        add(&session, false, &vout, "camera output")?;
        if mic.is_some() {
            let ac = class("AVCaptureAudioDataOutput")?;
            // SAFETY: +new.
            let aout: Option<Retained<AnyObject>> = unsafe { msg_send![ac, new] };
            let aout = aout.ok_or(CaptureError::Missing("AVCaptureAudioDataOutput"))?;
            // SAFETY: as for the video output.
            let () =
                unsafe { msg_send![&*aout, setSampleBufferDelegate: &*delegate, queue: &*queue] };
            add(&session, false, &aout, "mic output")?;
        }
        // SAFETY: closes the configuration opened above.
        let () = unsafe { msg_send![&*session, commitConfiguration] };

        let ctx = Arc::new(Ctx {
            writer: Mutex::new(Some(Writer::new(&cfg.path)?)),
            t0: AtomicU64::new(0),
            live: AtomicBool::new(true),
            video_output: AtomicUsize::new(&*vout as *const AnyObject as usize),
            with_mic: mic.is_some(),
            hevc: cfg.hevc,
            video: Mutex::new(None),
            audio: Mutex::new(None),
            failed: Mutex::new(None),
        });
        registry().insert(&*delegate as *const AnyObject as usize, ctx.clone());
        // SAFETY: -startRunning blocks until the devices run (never on the main thread
        // in the app: the recorder starts from a worker).
        let () = unsafe { msg_send![&*session, startRunning] };
        Ok(Self {
            camera: name_of(&cam),
            mic: mic.as_deref().map(name_of),
            session,
            delegate,
            _queue: queue,
            ctx,
        })
    }

    /// Capture samples rejected by the encoder so far. Never waits on a callback.
    pub fn dropped(&self) -> Option<u64> {
        let g = self.ctx.writer.try_lock().ok()?;
        g.as_ref().map(|w| w.dropped())
    }

    /// The camera's and the mic's names.
    pub fn devices(&self) -> (&str, Option<&str>) {
        (&self.camera, self.mic.as_deref())
    }

    /// Starts the file at host time `t0` (from the next frame on).
    pub fn go(&self, t0: u64) {
        self.ctx.t0.store(t0.max(1), Ordering::Release);
    }

    /// Stops the devices and closes the file.
    pub fn finish(self) -> Result<WriterStats, CaptureError> {
        // SAFETY: -stopRunning blocks until the devices stop; no callback runs after.
        let () = unsafe { msg_send![&*self.session, stopRunning] };
        self.ctx.live.store(false, Ordering::Release);
        registry().remove(&*self.delegate as *const AnyObject as usize);
        if let Some(m) = self.ctx.failed.lock().ok().and_then(|g| g.clone()) {
            return Err(CaptureError::Os {
                step: "face file",
                message: m,
            });
        }
        let writer = self.ctx.writer.lock().ok().and_then(|mut g| g.take());
        writer.ok_or(CaptureError::Missing("writer"))?.finish()
    }
}
