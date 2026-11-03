//! A screen through DXGI Desktop Duplication: the compositor hands over each new frame as a GPU
//! texture, copied once to a CPU texture and read back. About two milliseconds for a 1080p
//! screen, against twenty for GDI's `BitBlt` and `GetDIBits`, which is what makes 60 frames a
//! second possible. A frame comes only when the screen changed; otherwise the last one is
//! given again. Anything that refuses (a protected window, a full-screen exclusive game, a
//! driver that lost the output) is an `Err`, and the share falls back to GDI for that frame.
//! Windows only; a window share stays on `PrintWindow`.

use super::Rgba;
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

/// One screen being duplicated.
pub struct Duplicator {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
    /// The last frame read, given again while the screen does not change.
    last: Option<Rgba>,
}

impl Duplicator {
    /// Duplicates the screen whose DXGI output is `device` (`\\.\DISPLAY2`).
    pub fn open(device: &str) -> Result<Duplicator, String> {
        let wanted: Vec<u16> = device.encode_utf16().collect();
        // SAFETY: plain COM calls on objects the calls themselves hand back; every out pointer
        // is a local of the right type.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|err| err.to_string())?;
            let mut adapter_at = 0;
            while let Ok(adapter) = factory.EnumAdapters1(adapter_at) {
                adapter_at += 1;
                let mut output_at = 0;
                while let Ok(output) = adapter.EnumOutputs(output_at) {
                    output_at += 1;
                    let Ok(desc) = output.GetDesc() else { continue };
                    let len = desc.DeviceName.iter().position(|&c| c == 0).unwrap_or(desc.DeviceName.len());
                    if desc.DeviceName[..len] != wanted[..] {
                        continue;
                    }
                    let output: IDXGIOutput1 = output.cast().map_err(|err| err.to_string())?;
                    return Duplicator::on(&adapter, &output, desc.DesktopCoordinates.right - desc.DesktopCoordinates.left, desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top);
                }
            }
        }
        Err(format!("{device} is not a screen DXGI knows."))
    }

    unsafe fn on(adapter: &IDXGIAdapter1, output: &IDXGIOutput1, width: i32, height: i32) -> Result<Duplicator, String> {
        let (mut device, mut context) = (None, None);
        // SAFETY: as in `open`.
        unsafe {
            D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))
                .map_err(|err| err.to_string())?;
            let device = device.ok_or("No Direct3D device.")?;
            let context = context.ok_or("No Direct3D context.")?;
            let duplication = output.DuplicateOutput(&device).map_err(|err| err.to_string())?;
            Ok(Duplicator { device, context, duplication, staging: None, width: width.max(1) as u32, height: height.max(1) as u32, last: None })
        }
    }

    /// The screen now: a new frame when it changed, else the last one. `Err` when the output
    /// was lost (a mode change, a sign-out, a protected window): open it again.
    pub fn frame(&mut self) -> Result<Rgba, String> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: as in `open`; the frame is released before this returns, on every path.
        unsafe {
            match self.duplication.AcquireNextFrame(0, &mut info, &mut resource) {
                Ok(()) => {}
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    return self.last.clone().ok_or_else(|| "The screen has not drawn yet.".to_owned());
                }
                Err(err) if err.code() == DXGI_ERROR_ACCESS_LOST => return Err("The screen's output was lost.".to_owned()),
                Err(err) => return Err(err.to_string()),
            }
            let read = resource.ok_or("No frame.").and_then(|resource| resource.cast::<ID3D11Texture2D>().map_err(|_| "Not a texture.")).and_then(|texture| self.read(&texture));
            let _ = self.duplication.ReleaseFrame();
            // Kept for the frames in which nothing changes; one copy out, as for those.
            let last = self.last.insert(read.map_err(str::to_owned)?);
            Ok(last.clone())
        }
    }

    /// Copies `texture` to the CPU and reads it back as BGRA rows.
    unsafe fn read(&mut self, texture: &ID3D11Texture2D) -> Result<Rgba, &'static str> {
        // SAFETY: as in `open`; the mapped memory is read only between Map and Unmap, and only
        // inside the rows and pitch Direct3D reports.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                return Err("The screen is not BGRA.");
            }
            (self.width, self.height) = (desc.Width, desc.Height);
            if self.staging.as_ref().is_none_or(|staging| {
                let mut have = D3D11_TEXTURE2D_DESC::default();
                staging.GetDesc(&mut have);
                have.Width != desc.Width || have.Height != desc.Height
            }) {
                let staging = D3D11_TEXTURE2D_DESC {
                    Width: desc.Width,
                    Height: desc.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: desc.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut made = None;
                self.device.CreateTexture2D(&staging, None, Some(&mut made)).map_err(|_| "No staging texture.")?;
                self.staging = made;
            }
            let staging = self.staging.as_ref().ok_or("No staging texture.")?;
            self.context.CopyResource(staging, texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|_| "The frame could not be read.")?;
            let (width, height, pitch) = (desc.Width as usize, desc.Height as usize, mapped.RowPitch as usize);
            let mut pixels = Vec::with_capacity(width * height * 4);
            for row in 0..height {
                let from = std::slice::from_raw_parts((mapped.pData as *const u8).add(row * pitch), width * 4);
                pixels.extend_from_slice(from);
            }
            self.context.Unmap(staging, 0);
            Ok(Rgba { width: desc.Width, height: desc.Height, pixels, bgra: true })
        }
    }
}
