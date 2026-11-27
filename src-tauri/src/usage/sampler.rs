//! Low-frequency WGC, owned entirely by the native usage thread. No image
//! encoding, OCR, disk I/O, or full CPU image allocation occurs here.
use super::picture::{GRID_HEIGHT, GRID_LEN, GRID_WIDTH};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use windows::{
    core::{IInspectable, Interface},
    Foundation::{EventRegistrationToken, TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::IDXGIDevice,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
};

#[derive(Default)]
struct FrameSlot {
    frame: Option<Direct3D11CaptureFrame>,
    closed: bool,
}
type Latest = Arc<(Mutex<FrameSlot>, Condvar)>;
pub(super) struct SceneSampler {
    pub target: (isize, u32),
    pub interval_secs: u64,
    pub throttled: bool,
    d3d: ID3D11Device,
    context: ID3D11DeviceContext,
    _winrt: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: Option<EventRegistrationToken>,
    latest: Latest,
    staging: Option<ID3D11Texture2D>,
    size: windows::Graphics::SizeInt32,
    started: Instant,
}
impl SceneSampler {
    pub fn new(target: (isize, u32), interval_secs: u64) -> windows::core::Result<Self> {
        // SAFETY: native COM objects belong to this initialized MTA thread;
        // output pointers are initialized and are not shared with callbacks.
        unsafe {
            let (mut d3d, mut context) = (None, None);
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut context),
            )?;
            let d3d: ID3D11Device = d3d.ok_or_else(windows::core::Error::from_win32)?;
            let context = context.ok_or_else(windows::core::Error::from_win32)?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let interop =
                windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(HWND(target.0 as *mut _))?;
            let size = item.Size()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                size,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            let _ = session.SetIsBorderRequired(false);
            let _ = session.SetIsCursorCaptureEnabled(false);
            let requested = interval_secs as i64 * 10_000_000;
            let throttled = session
                .SetMinUpdateInterval(TimeSpan {
                    Duration: requested,
                })
                .is_ok()
                && session
                    .MinUpdateInterval()
                    .is_ok_and(|value| value.Duration >= requested);
            let latest: Latest = Arc::new((Mutex::new(FrameSlot::default()), Condvar::new()));
            let output = latest.clone();
            let token = pool.FrameArrived(&TypedEventHandler::new(
                move |pool: &Option<Direct3D11CaptureFramePool>, _: &Option<IInspectable>| {
                    if let Some(pool) = pool {
                        if let Ok(frame) = pool.TryGetNextFrame() {
                            let mut slot = output.0.lock().unwrap_or_else(|e| e.into_inner());
                            let old = if slot.closed {
                                Some(frame)
                            } else {
                                slot.frame.replace(frame)
                            };
                            drop(slot);
                            if let Some(old) = old {
                                let _ = old.Close();
                            }
                            output.1.notify_one();
                        }
                    }
                    Ok(())
                },
            ))?;
            let sampler = Self {
                target,
                interval_secs,
                throttled,
                d3d,
                context,
                _winrt: winrt,
                pool,
                session,
                token: Some(token),
                latest,
                staging: None,
                size,
                started: Instant::now(),
            };
            sampler.session.StartCapture()?;
            Ok(sampler)
        }
    }

    pub fn expired(&self) -> bool {
        self.started.elapsed() > Duration::from_secs(10 * 60)
    }
    pub fn poll(&mut self, wait: Duration) -> windows::core::Result<Option<(Vec<u8>, Instant)>> {
        let mut slot = self.latest.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.frame.is_none() && !wait.is_zero() {
            slot = self
                .latest
                .1
                .wait_timeout_while(slot, wait, |slot| slot.frame.is_none() && !slot.closed)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let frame = slot.frame.take();
        drop(slot);
        let Some(frame) = frame else {
            return Ok(None);
        };
        let at = frame_observed_at(&frame);
        let result = self.read_grid(&frame);
        let _ = frame.Close();
        result.map(|grid| Some((grid, at)))
    }

    fn read_grid(&mut self, frame: &Direct3D11CaptureFrame) -> windows::core::Result<Vec<u8>> {
        // SAFETY: the texture is mapped on its owning thread. Every sample is
        // inside the queried dimensions/row pitch; the mapping is always closed
        // before any fallible API call or backing resource release.
        unsafe {
            let content = frame.ContentSize()?;
            if content != self.size
                || content.Width <= 0
                || content.Height <= 0
                || content.Width > 8192
                || content.Height > 8192
            {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x80070057u32 as i32,
                )));
            }
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let source: ID3D11Texture2D = access.GetInterface()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut desc);
            let (width, height) = (content.Width as usize, content.Height as usize);
            if width > desc.Width as usize || height > desc.Height as usize {
                return Err(windows::core::Error::from_win32());
            }
            if self.staging.is_none() {
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
                desc.MiscFlags = 0;
                self.d3d
                    .CreateTexture2D(&desc, None, Some(&mut self.staging))?;
            }
            let staging = self.staging.as_ref().unwrap();
            self.context.CopyResource(staging, &source);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            if mapped.pData.is_null() || (mapped.RowPitch as usize) < width * 4 {
                self.context.Unmap(staging, 0);
                return Err(windows::core::Error::from_win32());
            }
            let raw = std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                mapped.RowPitch as usize * desc.Height as usize,
            );
            let mut grid = Vec::with_capacity(GRID_LEN);
            for gy in 0..GRID_HEIGHT {
                for gx in 0..GRID_WIDTH {
                    let mut sum = 0u32;
                    for oy in [1, 3] {
                        for ox in [1, 3] {
                            let x = ((gx * 4 + ox) * width / (GRID_WIDTH * 4)).min(width - 1);
                            let y = ((gy * 4 + oy) * height / (GRID_HEIGHT * 4)).min(height - 1);
                            let p = y * mapped.RowPitch as usize + x * 4;
                            sum += (29 * u32::from(raw[p])
                                + 150 * u32::from(raw[p + 1])
                                + 77 * u32::from(raw[p + 2]))
                                >> 8;
                        }
                    }
                    grid.push((sum / 4) as u8);
                }
            }
            self.context.Unmap(staging, 0);
            Ok(grid)
        }
    }
}
impl Drop for SceneSampler {
    fn drop(&mut self) {
        let frame = {
            let mut slot = self.latest.0.lock().unwrap_or_else(|e| e.into_inner());
            slot.closed = true;
            slot.frame.take()
        };
        if let Some(token) = self.token.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        let _ = self.session.Close();
        if let Some(frame) = frame {
            let _ = frame.Close();
        }
        let _ = self.pool.Close();
        self.staging = None;
        // SAFETY: no callbacks use the context. Clearing it drops driver-held
        // resource bindings before the entire device, rather than just a session.
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
    }
}

/// WGC timestamps use QPC in 100ns units. Keep acquisition time through the
/// normal screenshot cache instead of dating an old frame at its later read.
pub(crate) fn frame_observed_at(frame: &Direct3D11CaptureFrame) -> Instant {
    use std::sync::LazyLock;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    static FREQUENCY: LazyLock<i64> = LazyLock::new(|| {
        let mut frequency = 0;
        // SAFETY: Windows synchronously writes an initialized scalar.
        if unsafe { QueryPerformanceFrequency(&mut frequency) }.is_ok() {
            frequency
        } else {
            0
        }
    });
    let now = Instant::now();
    let Ok(frame_time) = frame.SystemRelativeTime() else {
        return now;
    };
    let mut counter = 0;
    // SAFETY: Windows synchronously writes an initialized scalar.
    if *FREQUENCY <= 0 || unsafe { QueryPerformanceCounter(&mut counter) }.is_err() {
        return now;
    }
    let age = (i128::from(counter) * 10_000_000 / i128::from(*FREQUENCY)
        - i128::from(frame_time.Duration))
    .max(0)
    .min(u64::MAX as i128 / 100) as u64;
    now.checked_sub(Duration::from_nanos(age * 100))
        .unwrap_or(now)
}
