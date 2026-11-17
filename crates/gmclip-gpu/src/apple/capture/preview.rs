//! The Record sheet's camera preview: the camera at its current mode, as the face file
//! records it, scaled to a small RGBA picture the sheet draws on a canvas. The webview's
//! own camera (`getUserMedia`) picks a mode of its own, and a capture card (Cam Link)
//! sends nothing in a mode other than its input's. So the output asks for the face
//! file's pixel format and nothing else (on macOS a size in its settings picks a camera
//! mode to match), and the pictures are scaled here.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::sel;

use super::face::{add, device_input};
use super::sources::av_device;
use super::{
    CMSampleBufferGetImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, CaptureError,
    Registry, class, ns_dict, ns_number_i64, serial_queue,
};

unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const AnyObject;
    fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetPixelFormatType(pb: *mut c_void) -> u32;
    fn CVPixelBufferGetPlaneCount(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(pb: *mut c_void, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(pb: *mut c_void, plane: usize) -> usize;
}

/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` ('420v'), as the face file records.
const PIXEL_420V: u32 = 0x3432_3076;
/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` ('420f').
const PIXEL_420F: u32 = 0x3432_3066;
/// `kCVPixelBufferLock_ReadOnly`.
const LOCK_READ: u64 = 1;
/// At most one picture per this many milliseconds (about 15 a second).
const EVERY_MS: u64 = 66;

/// One preview picture: RGBA, `w × h`, and how many have been seen (to skip repeats).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewFrame {
    pub w: u32,
    pub h: u32,
    pub seq: u64,
    pub rgba: Vec<u8>,
}

struct Ctx {
    latest: Mutex<Option<PreviewFrame>>,
    seq: AtomicU64,
    /// The width handed on; the height keeps the camera's shape.
    width: u32,
    start: Instant,
    /// When the last picture was taken: milliseconds since `start`, plus 1 (0: none yet).
    last_ms: AtomicU64,
}

fn px(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// A biplanar 4:2:0 picture (`420v`/`420f`, BT.709) as RGBA `width` wide, one sample per
/// output pixel.
///
/// # Safety
/// `pb` is a live pixel buffer or NULL.
unsafe fn scaled_rgba(pb: *mut c_void, width: u32) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: the caller's pixel buffer (or NULL, checked), locked read-only around the
    // reads and unlocked after; every read is inside a plane (rows and widths checked).
    unsafe {
        if pb.is_null() {
            return None;
        }
        let full = match CVPixelBufferGetPixelFormatType(pb) {
            PIXEL_420V => false,
            PIXEL_420F => true,
            _ => return None,
        };
        if CVPixelBufferGetPlaneCount(pb) < 2 || CVPixelBufferLockBaseAddress(pb, LOCK_READ) != 0 {
            return None;
        }
        let (sw, sh) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
        let (yp, cp) = (
            CVPixelBufferGetBaseAddressOfPlane(pb, 0),
            CVPixelBufferGetBaseAddressOfPlane(pb, 1),
        );
        let (ys, cs) = (
            CVPixelBufferGetBytesPerRowOfPlane(pb, 0),
            CVPixelBufferGetBytesPerRowOfPlane(pb, 1),
        );
        let ok = !yp.is_null()
            && !cp.is_null()
            && sw > 0
            && sh > 0
            && ys >= sw
            && cs >= sw.div_ceil(2) * 2;
        let out = ok.then(|| {
            let ow = (width as usize).clamp(1, sw);
            let oh = (sh * ow / sw).max(1);
            // video range: Y 16–235, Cb/Cr 16–240
            let (y0, ky, kc) = if full {
                (0.0, 1.0, 1.0)
            } else {
                (16.0, 255.0 / 219.0, 255.0 / 224.0)
            };
            let mut rgba = Vec::with_capacity(ow * oh * 4);
            for oy in 0..oh {
                let sy = (2 * oy + 1) * sh / (2 * oh);
                let (yrow, crow) = (yp.add(sy * ys), cp.add(sy / 2 * cs));
                for ox in 0..ow {
                    let sx = (2 * ox + 1) * sw / (2 * ow);
                    let l = (f32::from(*yrow.add(sx)) - y0) * ky;
                    let cb = (f32::from(*crow.add(sx / 2 * 2)) - 128.0) * kc;
                    let cr = (f32::from(*crow.add(sx / 2 * 2 + 1)) - 128.0) * kc;
                    rgba.extend_from_slice(&[
                        px(l + 1.5748 * cr),
                        px(l - 0.1873 * cb - 0.4681 * cr),
                        px(l + 1.8556 * cb),
                        255,
                    ]);
                }
            }
            (ow as u32, oh as u32, rgba)
        });
        CVPixelBufferUnlockBaseAddress(pb, LOCK_READ);
        out
    }
}

fn registry() -> &'static Registry<Ctx> {
    static R: Registry<Ctx> = Registry::new();
    &R
}

extern "C-unwind" fn did_output(
    this: &AnyObject,
    _: Sel,
    _output: *mut AnyObject,
    sbuf: *mut c_void,
    _conn: *mut AnyObject,
) {
    let Some(ctx) = registry().get(this as *const AnyObject as usize) else {
        return;
    };
    if sbuf.is_null() {
        return;
    }
    let now = ctx.start.elapsed().as_millis() as u64 + 1;
    let last = ctx.last_ms.load(Ordering::Acquire);
    if last != 0 && now < last + EVERY_MS {
        return;
    }
    ctx.last_ms.store(now, Ordering::Release);
    // SAFETY: a live camera sample buffer's pixel buffer (or NULL).
    let Some((w, h, rgba)) =
        (unsafe { scaled_rgba(CMSampleBufferGetImageBuffer(sbuf), ctx.width) })
    else {
        return;
    };
    let frame = PreviewFrame {
        w,
        h,
        seq: ctx.seq.fetch_add(1, Ordering::AcqRel) + 1,
        rgba,
    };
    if let Ok(mut g) = ctx.latest.lock() {
        *g = Some(frame);
    }
}

fn delegate_class() -> Result<&'static AnyClass, CaptureError> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            let superclass = AnyClass::get(c"NSObject")?;
            let mut b = ClassBuilder::new(c"GmclipPreviewOutput", superclass)?;
            if let Some(p) = AnyProtocol::get(c"AVCaptureVideoDataOutputSampleBufferDelegate") {
                b.add_protocol(p);
            }
            // SAFETY: matches -captureOutput:didOutputSampleBuffer:fromConnection:.
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

/// A running camera preview; stops when dropped.
pub struct CameraPreview {
    session: Retained<AnyObject>,
    delegate: Retained<AnyObject>,
    _queue: Retained<AnyObject>,
    ctx: Arc<Ctx>,
}

impl std::fmt::Debug for CameraPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CameraPreview").finish_non_exhaustive()
    }
}

// SAFETY: as `FaceRecorder`: the session is messaged from the owner only; the callback
// reaches shared state through `Ctx`'s mutex and atomic.
unsafe impl Send for CameraPreview {}
// SAFETY: `&self` only reads `Ctx` (a mutex and an atomic).
unsafe impl Sync for CameraPreview {}

impl CameraPreview {
    /// Starts the camera (`camera`: a unique id, else the default) in its current mode,
    /// handing pictures scaled to `width` (and the camera's shape).
    pub fn start(camera: Option<&str>, width: u32) -> Result<Self, CaptureError> {
        if !super::permissions().camera.granted() {
            return Err(CaptureError::Permission("Camera"));
        }
        let cam = av_device(true, camera)?;
        let sc = class("AVCaptureSession")?;
        // SAFETY: +new.
        let session: Option<Retained<AnyObject>> = unsafe { msg_send![sc, new] };
        let session = session.ok_or(CaptureError::Missing("AVCaptureSession"))?;
        let dc = delegate_class()?;
        // SAFETY: +new on the NSObject subclass registered above.
        let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![dc, new] };
        let delegate = delegate.ok_or(CaptureError::Missing("GmclipPreviewOutput"))?;
        let queue = serial_queue(c"net.gmclip.capture.preview")?;

        // SAFETY: configuration brackets on a new session.
        let () = unsafe { msg_send![&*session, beginConfiguration] };
        add(&session, true, &*device_input(&cam)?, "camera input")?;
        let vc = class("AVCaptureVideoDataOutput")?;
        // SAFETY: +new.
        let vout: Option<Retained<AnyObject>> = unsafe { msg_send![vc, new] };
        let vout = vout.ok_or(CaptureError::Missing("AVCaptureVideoDataOutput"))?;
        // SAFETY: a CoreVideo constant key; plain setters; the output keeps the delegate
        // weakly (held here) and the queue strongly.
        unsafe {
            let fmt = ns_number_i64(i64::from(PIXEL_420V))?;
            let settings = ns_dict(&[(&*kCVPixelBufferPixelFormatTypeKey, &*fmt)])?;
            let () = msg_send![&*vout, setVideoSettings: &*settings];
            let () = msg_send![&*vout, setAlwaysDiscardsLateVideoFrames: Bool::YES];
            let () = msg_send![&*vout, setSampleBufferDelegate: &*delegate, queue: &*queue];
        }
        add(&session, false, &vout, "camera output")?;
        // SAFETY: closes the configuration opened above.
        let () = unsafe { msg_send![&*session, commitConfiguration] };

        let ctx = Arc::new(Ctx {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            width,
            start: Instant::now(),
            last_ms: AtomicU64::new(0),
        });
        registry().insert(&*delegate as *const AnyObject as usize, ctx.clone());
        // SAFETY: -startRunning blocks until the camera runs (called from a worker).
        let () = unsafe { msg_send![&*session, startRunning] };
        Ok(Self {
            session,
            delegate,
            _queue: queue,
            ctx,
        })
    }

    /// The newest picture, when it is newer than `after` (a `seq` seen before).
    pub fn latest(&self, after: u64) -> Option<PreviewFrame> {
        let g = self.ctx.latest.lock().ok()?;
        g.as_ref().filter(|f| f.seq > after).cloned()
    }
}

impl Drop for CameraPreview {
    fn drop(&mut self) {
        // SAFETY: -stopRunning blocks until the camera stops; no callback runs after.
        let () = unsafe { msg_send![&*self.session, stopRunning] };
        registry().remove(&*self.delegate as *const AnyObject as usize);
    }
}
