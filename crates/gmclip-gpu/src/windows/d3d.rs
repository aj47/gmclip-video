//! The D3D11 device Media Foundation's hardware decoders and encoders run on (M5): one
//! per process, on the high-performance adapter (the discrete GPU wgpu also picks; this
//! PC has an integrated one too), multithread-protected (decoders on several threads share it),
//! behind an `IMFDXGIDeviceManager`. Decoded pictures are textures; [`D3dDevice::read_nv12`]
//! copies one to the CPU as packed NV12 (the planes the compositor uploads today; a
//! zero-copy DX12 import is the next step).

use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, IDXGIAdapter, IDXGIAdapter1,
    IDXGIFactory6,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};

use windows::core::Interface;

use super::mf::{ensure, os};
use crate::GpuError;

/// A staging texture for one picture size and format.
struct Staging {
    size: (u32, u32),
    format: DXGI_FORMAT,
    tex: ID3D11Texture2D,
}

/// The process's D3D11 device for Media Foundation.
pub struct D3dDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
    staging: Mutex<Option<Staging>>,
    /// The adapter's LUID (`(HighPart << 32) | LowPart`, as `MFT_ENUM_ADAPTER_LUID`).
    luid: u64,
    /// The adapter's name, as DXGI reports it ("NVIDIA GeForce RTX 5080").
    name: String,
}

// SAFETY: the device is multithread-protected (`ID3D10Multithread`), so its immediate
// context serialises calls from any thread; the staging slot is behind a mutex; the
// device manager is free-threaded (it locks the device for MF).
unsafe impl Send for D3dDevice {}
// SAFETY: as above: every use through `&self` is serialised by the device or the mutex.
unsafe impl Sync for D3dDevice {}

impl std::fmt::Debug for D3dDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dDevice").finish_non_exhaustive()
    }
}

impl D3dDevice {
    /// The process's device, made on first use; `Err` (every time) when this machine
    /// has no hardware D3D11 device with video support.
    pub fn shared() -> Result<Arc<D3dDevice>, GpuError> {
        static DEV: OnceLock<Result<Arc<D3dDevice>, String>> = OnceLock::new();
        DEV.get_or_init(|| D3dDevice::new().map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(GpuError::Unsupported)
    }

    fn new() -> Result<Self, GpuError> {
        ensure()?;
        let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        // the high-performance adapter, when DXGI can name one; else the default
        // SAFETY: plain factory/adapter calls; the interfaces are released by their
        // wrappers.
        let adapter: Option<IDXGIAdapter1> = unsafe {
            CreateDXGIFactory1::<IDXGIFactory6>().ok().and_then(|f| {
                f.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
                    .ok()
            })
        };
        // SAFETY: a live adapter; plain description read.
        let desc = adapter.as_ref().and_then(|a| unsafe { a.GetDesc1() }.ok());
        let luid = desc.as_ref().map_or(0, |d| {
            (u64::from(d.AdapterLuid.HighPart as u32) << 32) | u64::from(d.AdapterLuid.LowPart)
        });
        let name = desc.as_ref().map_or_else(String::new, |d| {
            let n = d
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(d.Description.len());
            String::from_utf16_lossy(&d.Description[..n])
        });
        let base: Option<IDXGIAdapter> = adapter.as_ref().and_then(|a| a.cast().ok());
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: plain creation call; the out-pointers are locals that outlive it.
        unsafe {
            D3D11CreateDevice(
                base.as_ref(),
                if base.is_some() {
                    D3D_DRIVER_TYPE_UNKNOWN
                } else {
                    D3D_DRIVER_TYPE_HARDWARE
                },
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| os("D3D11CreateDevice(video)", &e))?;
        let (device, context) = device
            .zip(context)
            .ok_or_else(|| GpuError::Device("D3D11CreateDevice gave no device".into()))?;
        // SAFETY: a live device; the multithread interface is released by its wrapper.
        unsafe {
            let mt: ID3D11Multithread = device.cast().map_err(|e| os("ID3D11Multithread", &e))?;
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        // SAFETY: MF is started; the out-pointers are locals; the device outlives the
        // manager's use of it (both live in `Self`).
        unsafe {
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .map_err(|e| os("MFCreateDXGIDeviceManager", &e))?;
        }
        let manager = manager.ok_or_else(|| GpuError::Device("no DXGI device manager".into()))?;
        // SAFETY: a live manager and device; `token` is the one the manager gave.
        unsafe { manager.ResetDevice(&device, token) }
            .map_err(|e| os("IMFDXGIDeviceManager::ResetDevice", &e))?;
        Ok(Self {
            device,
            context,
            manager,
            staging: Mutex::new(None),
            luid,
            name,
        })
    }

    /// The device's adapter LUID (0 when DXGI did not report one).
    pub fn luid(&self) -> u64 {
        self.luid
    }

    /// The device's adapter name ("" when DXGI did not report one).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The device manager handed to a decoder MFT (`MFT_MESSAGE_SET_D3D_MANAGER`).
    pub fn manager(&self) -> &IMFDXGIDeviceManager {
        &self.manager
    }

    /// Copies subresource `index` of an NV12 or P010 texture to the CPU, cropped to
    /// `(x, y, w, h)`, as packed planes (`w × h` luma, `w × h / 2` interleaved CbCr
    /// samples; 2 bytes a sample for P010).
    pub fn read_nv12(
        &self,
        tex: &ID3D11Texture2D,
        index: u32,
        (x, y, w, h): (u32, u32, u32, u32),
    ) -> Result<(Vec<u8>, Vec<u8>), GpuError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture; `desc` is a local it fills.
        unsafe { tex.GetDesc(&mut desc) };
        let bps = match desc.Format {
            f if f == DXGI_FORMAT_NV12 => 1usize,
            f if f == DXGI_FORMAT_P010 => 2,
            _ => 0,
        };
        if bps == 0 || x + w > desc.Width || y + h > desc.Height {
            return Err(GpuError::Unsupported(format!(
                "a decoded texture {:?} {}x{}, crop {x},{y} {w}x{h}",
                desc.Format, desc.Width, desc.Height
            )));
        }
        let size = (desc.Width, desc.Height);
        let mut slot = self
            .staging
            .lock()
            .map_err(|_| GpuError::Device("staging lock poisoned".into()))?;
        if slot
            .as_ref()
            .is_none_or(|s| s.size != size || s.format != desc.Format)
        {
            let sd = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut t: Option<ID3D11Texture2D> = None;
            // SAFETY: a live device; `sd` describes a valid staging texture; `t` is a
            // local out-pointer.
            unsafe { self.device.CreateTexture2D(&sd, None, Some(&mut t)) }
                .map_err(|e| os("ID3D11Device::CreateTexture2D(staging)", &e))?;
            *slot = Some(Staging {
                size,
                format: desc.Format,
                tex: t.ok_or_else(|| GpuError::Device("no staging texture".into()))?,
            });
        }
        let staging = &slot.as_ref().expect("just made").tex;
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are live, the same size and format (checked above), and
        // `index` names a subresource of `tex` (the decoder's); the staging texture is
        // mapped for reading only while the copy below runs, and unmapped after it.
        unsafe {
            self.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, tex, index, None);
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut map))
                .map_err(|e| os("ID3D11DeviceContext::Map(staging)", &e))?;
        }
        let pitch = map.RowPitch as usize;
        let rows = size.1 as usize;
        let (xu, yu, wu, hu) = (x as usize, y as usize, w as usize, h as usize);
        let mut py = Vec::with_capacity(wu * hu * bps);
        let mut puv = Vec::with_capacity(wu * hu / 2 * bps);
        // SAFETY: the mapping holds `rows` luma rows then `rows / 2` chroma rows of
        // `pitch ≥ width` bytes (NV12 staging layout); every slice read below lies inside
        // it (the crop is within the texture, checked above).
        unsafe {
            let base = map.pData as *const u8;
            for r in yu..yu + hu {
                py.extend_from_slice(std::slice::from_raw_parts(
                    base.add(r * pitch + xu * bps),
                    wu * bps,
                ));
            }
            let uv = base.add(pitch * rows);
            for r in yu / 2..(yu + hu) / 2 {
                puv.extend_from_slice(std::slice::from_raw_parts(
                    uv.add(r * pitch + (xu & !1) * bps),
                    wu * bps,
                ));
            }
            self.context.Unmap(staging, 0);
        }
        Ok((py, puv))
    }
}
