//! Media Foundation plumbing shared by the decoders (M5): the process's `MFStartup`, COM
//! on the calling thread, sample/buffer helpers. COM is entered in the multithreaded
//! apartment once per thread and never left (the MFTs are free-threaded and may be
//! called from any MTA thread afterwards); a thread already in an STA keeps it.

use std::cell::Cell;
use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{
    IMFMediaBuffer, IMFSample, MF_VERSION, MFCreateMemoryBuffer, MFCreateSample, MFSTARTUP_FULL,
    MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

use crate::GpuError;

pub(crate) fn os(step: &'static str, e: &windows::core::Error) -> GpuError {
    GpuError::Os {
        step,
        status: i64::from(e.code().0),
    }
}

thread_local! {
    static COM: Cell<bool> = const { Cell::new(false) };
}

/// COM (MTA) on this thread and Media Foundation in this process, once each.
pub(crate) fn ensure() -> Result<(), GpuError> {
    if !COM.with(Cell::get) {
        // SAFETY: plain call. Deliberately never balanced by CoUninitialize: MF objects
        // made on this thread may live as long as it does.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        COM.with(|c| c.set(true));
    }
    static MF: OnceLock<Result<(), i32>> = OnceLock::new();
    // SAFETY: plain call, once per process; never shut down (the process ends first).
    let r =
        MF.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.code().0));
    r.map_err(|status| GpuError::Os {
        step: "MFStartup",
        status: i64::from(status),
    })
}

/// A sample holding a copy of `data`.
pub(crate) fn sample_of(data: &[u8], time_hns: i64) -> Result<IMFSample, GpuError> {
    let len =
        u32::try_from(data.len()).map_err(|_| GpuError::Unsupported("packet too large".into()))?;
    // SAFETY: MF is started (callers run `ensure` first); the buffer is locked only for
    // the copy, `len` bytes, which it was created with room for.
    unsafe {
        let buf: IMFMediaBuffer =
            MFCreateMemoryBuffer(len.max(1)).map_err(|e| os("MFCreateMemoryBuffer", &e))?;
        let mut p: *mut u8 = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)
            .map_err(|e| os("IMFMediaBuffer::Lock", &e))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        buf.Unlock().map_err(|e| os("IMFMediaBuffer::Unlock", &e))?;
        buf.SetCurrentLength(len)
            .map_err(|e| os("IMFMediaBuffer::SetCurrentLength", &e))?;
        let s: IMFSample = MFCreateSample().map_err(|e| os("MFCreateSample", &e))?;
        s.AddBuffer(&buf)
            .map_err(|e| os("IMFSample::AddBuffer", &e))?;
        s.SetSampleTime(time_hns)
            .map_err(|e| os("IMFSample::SetSampleTime", &e))?;
        Ok(s)
    }
}

/// An empty sample with one buffer of `cap` bytes, for an MFT that does not allocate
/// its own output.
pub(crate) fn empty_sample(cap: u32) -> Result<IMFSample, GpuError> {
    // SAFETY: MF is started; plain object creation.
    unsafe {
        let buf: IMFMediaBuffer =
            MFCreateMemoryBuffer(cap.max(1)).map_err(|e| os("MFCreateMemoryBuffer", &e))?;
        let s: IMFSample = MFCreateSample().map_err(|e| os("MFCreateSample", &e))?;
        s.AddBuffer(&buf)
            .map_err(|e| os("IMFSample::AddBuffer", &e))?;
        Ok(s)
    }
}

/// Appends a sample's bytes (all its buffers, contiguous) to `out`.
pub(crate) fn append_bytes(s: &IMFSample, out: &mut Vec<u8>) -> Result<(), GpuError> {
    // SAFETY: a live sample; its contiguous buffer is locked only for the copy of the
    // `len` valid bytes it reports.
    unsafe {
        let buf = s
            .ConvertToContiguousBuffer()
            .map_err(|e| os("IMFSample::ConvertToContiguousBuffer", &e))?;
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len))
            .map_err(|e| os("IMFMediaBuffer::Lock", &e))?;
        out.extend_from_slice(std::slice::from_raw_parts(p, len as usize));
        buf.Unlock().map_err(|e| os("IMFMediaBuffer::Unlock", &e))?;
    }
    Ok(())
}
