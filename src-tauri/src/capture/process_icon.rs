//! Convert local executable icons to small, straight-alpha PNGs.
use base64::Engine;
use image::ImageEncoder;
use windows::{
    core::PCWSTR,
    Win32::{
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
        },
        UI::{
            Shell::ExtractIconExW,
            WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_MASK, DI_NORMAL, HICON},
        },
    },
};

const ICON_SIZE: i32 = 32;
const PIXEL_BYTES: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;

struct ExtractedIcons([HICON; 2]);

impl Drop for ExtractedIcons {
    fn drop(&mut self) {
        // SAFETY: extraction transfers ownership of these handles. Invalid or
        // duplicate handles are skipped, including on early error returns.
        unsafe {
            if !self.0[0].is_invalid() {
                let _ = DestroyIcon(self.0[0]);
            }
            if !self.0[1].is_invalid() && self.0[1] != self.0[0] {
                let _ = DestroyIcon(self.0[1]);
            }
        }
    }
}

struct IconBitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
}

impl IconBitmap {
    fn new() -> Option<Self> {
        // SAFETY: the DIB dimensions are fixed, its DC and bitmap are owned by
        // this guard, and Drop restores the selection before deleting them.
        unsafe {
            let mut surface = Self {
                dc: CreateCompatibleDC(None),
                bitmap: HBITMAP::default(),
                previous: HGDIOBJ::default(),
                bits: std::ptr::null_mut(),
            };
            if surface.dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: ICON_SIZE,
                    biHeight: -ICON_SIZE,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            surface.bitmap =
                CreateDIBSection(surface.dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            surface.bits = bits.cast();
            if surface.bits.is_null() {
                return None;
            }
            surface.previous = SelectObject(surface.dc, surface.bitmap);
            if surface.previous.is_invalid() {
                return None;
            }
            Some(surface)
        }
    }
}

impl Drop for IconBitmap {
    fn drop(&mut self) {
        // SAFETY: these handles belong to this surface, and the original
        // bitmap is restored before the owned bitmap and DC are released.
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap);
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

fn straight_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel.fill(0);
        } else {
            pixel.swap(0, 2);
            // DrawIconEx uses premultiplied BGRA; PNG stores straight RGBA.
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

fn icon_to_png(icon: HICON) -> Option<Vec<u8>> {
    let surface = IconBitmap::new()?;
    // SAFETY: this is the checked, fixed-size DIB allocation owned by surface.
    // GDI is flushed before reading or modifying its memory, and no slice
    // survives the guard that owns the bitmap.
    let mut pixels = unsafe {
        std::ptr::write_bytes(surface.bits, 0, PIXEL_BYTES);
        DrawIconEx(
            surface.dc, 0, 0, icon, ICON_SIZE, ICON_SIZE, 0, None, DI_NORMAL,
        )
        .ok()?;
        if !GdiFlush().as_bool() {
            return None;
        }
        let mut pixels = std::slice::from_raw_parts(surface.bits, PIXEL_BYTES).to_vec();
        if !pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
            // Older icons use an AND mask rather than an alpha channel.
            // Recover that mask so opaque black pixels remain visible.
            std::ptr::write_bytes(surface.bits, 0, PIXEL_BYTES);
            DrawIconEx(
                surface.dc, 0, 0, icon, ICON_SIZE, ICON_SIZE, 0, None, DI_MASK,
            )
            .ok()?;
            if !GdiFlush().as_bool() {
                return None;
            }
            let mask = std::slice::from_raw_parts(surface.bits, PIXEL_BYTES);
            for (pixel, mask) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                pixel[3] = if mask[0] == 0 { 255 } else { 0 };
            }
        }
        pixels
    };
    straight_rgba(&mut pixels);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &pixels,
            ICON_SIZE as u32,
            ICON_SIZE as u32,
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(png)
}

pub(crate) fn extract_process_icon_base64(exe_path: &str) -> Option<String> {
    let path: Vec<_> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut icons = ExtractedIcons([HICON::default(); 2]);
    // SAFETY: the path is NUL-terminated and the two output slots stay live
    // during extraction. The guard owns both returned handles on every path.
    let count = unsafe {
        ExtractIconExW(
            PCWSTR(path.as_ptr()),
            0,
            Some(&mut icons.0[0]),
            Some(&mut icons.0[1]),
            1,
        )
    };
    if count == 0 {
        return None;
    }
    let icon = icons.0.iter().copied().find(|icon| !icon.is_invalid())?;
    Some(base64::engine::general_purpose::STANDARD.encode(icon_to_png(icon)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};

    fn native_icon(alpha: bool) -> ExtractedIcons {
        let size = ICON_SIZE as usize;
        let stride = if alpha { size * 4 } else { size * 3 };
        let mask_stride = size.div_ceil(32) * 4;
        let mut resource = vec![0; 40 + stride * size + mask_stride * size];
        resource[0..4].copy_from_slice(&40u32.to_le_bytes());
        resource[4..8].copy_from_slice(&ICON_SIZE.to_le_bytes());
        resource[8..12].copy_from_slice(&(ICON_SIZE * 2).to_le_bytes());
        resource[12..14].copy_from_slice(&1u16.to_le_bytes());
        resource[14..16].copy_from_slice(&(if alpha { 32u16 } else { 24u16 }).to_le_bytes());
        for y in 0..size {
            for x in 0..size {
                let row = size - 1 - y;
                if x == 0 || y == 0 || x == size - 1 || y == size - 1 {
                    resource[40 + stride * size + row * mask_stride + x / 8] |= 0x80 >> (x % 8);
                    continue;
                }
                let offset = 40 + row * stride + x * if alpha { 4 } else { 3 };
                if x < size / 2 {
                    resource[offset..offset + 3].copy_from_slice(&[64, 128, 255]);
                    if alpha {
                        resource[offset + 3] = 128;
                    }
                } else if alpha {
                    resource[offset + 3] = 255;
                }
            }
        }
        // SAFETY: resource contains a complete, bounded icon DIB and AND mask;
        // Windows copies it into the returned icon owned by the guard.
        let icon = unsafe {
            CreateIconFromResourceEx(
                &resource,
                true,
                0x0003_0000,
                ICON_SIZE,
                ICON_SIZE,
                LR_DEFAULTCOLOR,
            )
            .unwrap()
        };
        ExtractedIcons([icon, HICON::default()])
    }

    #[test]
    fn partial_alpha_restores_color_without_changing_opaque_or_transparent_pixels() {
        let mut pixels = [32, 64, 128, 128, 7, 11, 13, 255, 17, 19, 23, 0];
        straight_rgba(&mut pixels);
        assert_eq!(pixels, [255, 128, 64, 128, 13, 11, 7, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn native_alpha_icon_round_trips_as_straight_rgba_at_the_existing_size() {
        let icons = native_icon(true);
        let png = icon_to_png(icons.0[0]).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (32, 32));
        assert_eq!(decoded.get_pixel(8, 16).0, [255, 128, 64, 128]);
        assert_eq!(decoded.get_pixel(24, 16).0, [0, 0, 0, 255]);
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }

    #[test]
    fn native_legacy_mask_preserves_colors_and_opaque_black_pixels() {
        let icons = native_icon(false);
        let png = icon_to_png(icons.0[0]).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(8, 16).0, [255, 128, 64, 255]);
        assert_eq!(decoded.get_pixel(24, 16).0, [0, 0, 0, 255]);
        assert_eq!(decoded.get_pixel(0, 0).0, [0, 0, 0, 0]);
    }
}
