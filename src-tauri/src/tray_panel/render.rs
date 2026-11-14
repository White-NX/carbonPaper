//! Direct2D/DirectWrite resources stay on the window thread. Device resources
//! and fonts are prepared while hidden; protected bitmaps follow read identity.

use super::model::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use windows::core::{w, Error, Interface, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{E_FAIL, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub background: D2D1_COLOR_F,
    pub paper: D2D1_COLOR_F,
    pub surface: D2D1_COLOR_F,
    hover: D2D1_COLOR_F,
    line: D2D1_COLOR_F,
    pub text: D2D1_COLOR_F,
    pub muted: D2D1_COLOR_F,
    accent: D2D1_COLOR_F,
    accent_surface: D2D1_COLOR_F,
    success: D2D1_COLOR_F,
    warning: D2D1_COLOR_F,
    error: D2D1_COLOR_F,
}

fn rgb(hex: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 255) as f32 / 255.0,
        g: ((hex >> 8) & 255) as f32 / 255.0,
        b: (hex & 255) as f32 / 255.0,
        a: 1.0,
    }
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                background: rgb(0x252526),
                paper: rgb(0x1e1e1e),
                surface: rgb(0x252526),
                hover: rgb(0x2a2d2e),
                line: rgb(0x333333),
                text: rgb(0xcccccc),
                muted: rgb(0x858585),
                accent: rgb(0x007acc),
                accent_surface: rgb(0x20313d),
                success: rgb(0x89d185),
                warning: rgb(0xf59e0b),
                error: rgb(0xf14c4c),
            }
        } else {
            Self {
                background: rgb(0xffffff),
                paper: rgb(0xf7f9fc),
                surface: rgb(0xffffff),
                hover: rgb(0xe5eaf1),
                line: rgb(0xd9dde7),
                text: rgb(0x1f2430),
                muted: rgb(0x6b7280),
                accent: rgb(0x2563eb),
                accent_surface: rgb(0xe0e9fc),
                success: rgb(0x16a34a),
                warning: rgb(0xb45309),
                error: rgb(0xdc2626),
            }
        }
    }
}

fn blend(first: D2D1_COLOR_F, second: D2D1_COLOR_F, amount: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: first.r + (second.r - first.r) * amount,
        g: first.g + (second.g - first.g) * amount,
        b: first.b + (second.b - first.b) * amount,
        a: 1.0,
    }
}

fn bounds(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x,
        top: rect.y,
        right: rect.x + rect.w,
        bottom: rect.y + rect.h,
    }
}

struct Painter {
    target: ID2D1RenderTarget,
    brush: ID2D1SolidColorBrush,
    write: IDWriteFactory,
    fonts: BTreeMap<(u16, bool), IDWriteTextFormat>,
    bitmaps: BTreeMap<i64, (Arc<Thumbnail>, ID2D1Bitmap)>,
    marks: [ID2D1Bitmap; 2],
}

impl Painter {
    unsafe fn new(target: ID2D1RenderTarget, write: IDWriteFactory) -> Result<Self> {
        let brush = target.CreateSolidColorBrush(&rgb(0), None)?;
        // This raster is generated from the same carbonpaper-mark.svg used by
        // BrandMark in the main window; only its alpha mask is recolored.
        let mark =
            image::load_from_memory(include_bytes!("../../../src/assets/carbonpaper-mark.png"))
                .map_err(|error| {
                    Error::new(E_FAIL, format!("Invalid bundled brand mark: {error}"))
                })?
                .to_rgba8();
        let (width, height) = mark.dimensions();
        let make_mark = |dark| -> Result<ID2D1Bitmap> {
            let color = Palette::new(dark).muted;
            let mut pixels = mark.clone().into_raw();
            for pixel in pixels.as_chunks_mut::<4>().0 {
                let alpha = f32::from(pixel[3]);
                pixel[0] = (color.b * alpha).round() as u8;
                pixel[1] = (color.g * alpha).round() as u8;
                pixel[2] = (color.r * alpha).round() as u8;
            }
            target.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(pixels.as_ptr().cast()),
                width * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                },
            )
        };
        let marks = [make_mark(false)?, make_mark(true)?];
        let mut painter = Self {
            target,
            brush,
            write,
            fonts: BTreeMap::new(),
            bitmaps: BTreeMap::new(),
            marks,
        };
        for (size, bold) in [
            (11, false),
            (12, false),
            (12, true),
            (13, false),
            (13, true),
            (14, true),
        ] {
            painter.font(size, bold)?;
        }
        Ok(painter)
    }

    unsafe fn font(&mut self, size: u16, bold: bool) -> Result<IDWriteTextFormat> {
        if let Some(font) = self.fonts.get(&(size, bold)) {
            return Ok(font.clone());
        }
        let font = self.write.CreateTextFormat(
            w!("Segoe UI"),
            None,
            if bold {
                DWRITE_FONT_WEIGHT_SEMI_BOLD
            } else {
                DWRITE_FONT_WEIGHT_NORMAL
            },
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            f32::from(size),
            w!(""),
        )?;
        font.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        self.fonts.insert((size, bold), font.clone());
        Ok(font)
    }

    unsafe fn text(
        &mut self,
        text: &str,
        rect: Rect,
        size: u16,
        bold: bool,
        color: D2D1_COLOR_F,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> Result<()> {
        let wide = Zeroizing::new(text.encode_utf16().collect::<Vec<_>>());
        let font = self.font(size, bold)?;
        let layout = self
            .write
            .CreateTextLayout(&wide, &font, rect.w.max(1.0), rect.h.max(1.0))?;
        layout.SetTextAlignment(align)?;
        layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let ellipsis = self.write.CreateEllipsisTrimmingSign(&font)?;
        layout.SetTrimming(
            &DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                ..Default::default()
            },
            &ellipsis,
        )?;
        self.brush.SetColor(&color);
        self.target.DrawTextLayout(
            D2D_POINT_2F {
                x: rect.x,
                y: rect.y,
            },
            &layout,
            &self.brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
        );
        Ok(())
    }

    unsafe fn rounded(&self, rect: Rect, radius: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: bounds(rect),
                radiusX: radius,
                radiusY: radius,
            },
            &self.brush,
        );
    }

    unsafe fn fill(&self, rect: Rect, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.FillRectangle(&bounds(rect), &self.brush);
    }

    unsafe fn measure(&mut self, text: &str, size: u16, bold: bool) -> Result<f32> {
        let wide: Vec<_> = text.encode_utf16().collect();
        let font = self.font(size, bold)?;
        let layout = self.write.CreateTextLayout(&wide, &font, 1024.0, 40.0)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        Ok(metrics.width)
    }

    unsafe fn mark(&self, dark: bool, rect: Rect) {
        self.target.DrawBitmap(
            &self.marks[usize::from(dark)],
            Some(&bounds(rect)),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
    }

    unsafe fn clock(&self, x: f32, y: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.DrawEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x, y },
                radiusX: 6.0,
                radiusY: 6.0,
            },
            &self.brush,
            1.2,
            None,
        );
        self.line((x, y - 3.5), (x, y), color, 1.2);
        self.line((x, y), (x + 2.5, y + 1.5), color, 1.2);
    }

    unsafe fn pause(&self, x: f32, y: f32, color: D2D1_COLOR_F) {
        self.line((x - 3.0, y - 5.0), (x - 3.0, y + 5.0), color, 1.5);
        self.line((x + 3.0, y - 5.0), (x + 3.0, y + 5.0), color, 1.5);
    }

    unsafe fn play(&self, x: f32, y: f32, color: D2D1_COLOR_F) {
        self.line((x - 3.0, y - 5.0), (x + 4.0, y), color, 1.3);
        self.line((x + 4.0, y), (x - 3.0, y + 5.0), color, 1.3);
        self.line((x - 3.0, y + 5.0), (x - 3.0, y - 5.0), color, 1.3);
    }

    unsafe fn lock(&self, x: f32, y: f32, color: D2D1_COLOR_F, background: D2D1_COLOR_F) {
        self.outline(Rect::new(x - 4.0, y - 8.0, 8.0, 11.0), 4.0, color);
        self.rounded(Rect::new(x - 6.0, y - 2.0, 12.0, 10.0), 2.0, background);
        self.outline(Rect::new(x - 6.0, y - 2.0, 12.0, 10.0), 2.0, color);
        self.dot(x, y + 2.0, 1.0, color);
        self.line((x, y + 2.0), (x, y + 4.5), color, 1.0);
    }

    unsafe fn outline(&self, rect: Rect, radius: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: bounds(rect),
                radiusX: radius,
                radiusY: radius,
            },
            &self.brush,
            1.0,
            None,
        );
    }

    unsafe fn line(&self, from: (f32, f32), to: (f32, f32), color: D2D1_COLOR_F, width: f32) {
        self.brush.SetColor(&color);
        self.target.DrawLine(
            D2D_POINT_2F {
                x: from.0,
                y: from.1,
            },
            D2D_POINT_2F { x: to.0, y: to.1 },
            &self.brush,
            width,
            None,
        );
    }

    unsafe fn dot(&self, x: f32, y: f32, radius: f32, color: D2D1_COLOR_F) {
        self.brush.SetColor(&color);
        self.target.FillEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x, y },
                radiusX: radius,
                radiusY: radius,
            },
            &self.brush,
        );
    }

    unsafe fn arrow(&self, x: f32, y: f32, color: D2D1_COLOR_F) {
        self.line((x - 5.0, y), (x + 5.0, y), color, 1.3);
        self.line((x + 1.0, y - 4.0), (x + 5.0, y), color, 1.3);
        self.line((x + 5.0, y), (x + 1.0, y + 4.0), color, 1.3);
    }

    unsafe fn prepare_thumbnail(&mut self, record: &Record) -> Result<()> {
        let Some(thumbnail) = &record.thumbnail else {
            return Ok(());
        };
        if self
            .bitmaps
            .get(&record.id)
            .is_none_or(|(old, _)| !Arc::ptr_eq(old, thumbnail))
        {
            let bitmap = self.target.CreateBitmap(
                D2D_SIZE_U {
                    width: thumbnail.width,
                    height: thumbnail.height,
                },
                Some(thumbnail.pixels.as_ptr().cast()),
                thumbnail.width * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                },
            )?;
            self.bitmaps.insert(record.id, (thumbnail.clone(), bitmap));
        }
        Ok(())
    }

    unsafe fn thumbnail(&mut self, record: &Record, rect: Rect) -> Result<()> {
        self.prepare_thumbnail(record)?;
        let Some(thumbnail) = &record.thumbnail else {
            return Ok(());
        };
        let (_, bitmap) = &self.bitmaps[&record.id];
        let ratio = (rect.w / thumbnail.width as f32).min(rect.h / thumbnail.height as f32);
        let width = thumbnail.width as f32 * ratio;
        let height = thumbnail.height as f32 * ratio;
        let destination = bounds(Rect::new(
            rect.x + (rect.w - width) / 2.0,
            rect.y + (rect.h - height) / 2.0,
            width,
            height,
        ));
        self.target.DrawBitmap(
            bitmap,
            Some(&destination),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        Ok(())
    }
}

pub(super) struct ControlVisual {
    pub id: u16,
    pub focused: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub enabled: bool,
}

pub(super) struct Renderer {
    root: ID2D1DCRenderTarget,
    root_painter: Painter,
    identity: Option<ReadIdentity>,
}

impl Renderer {
    pub unsafe fn new(_hwnd: HWND, _width: u32, _height: u32, dpi: f32) -> Result<Self> {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        // An opaque DC target composes with the native EDIT through the same
        // clipped GDI surface. Mixing GDI input with an alpha HWND target over
        // Acrylic previously let full-panel paints cover the input background.
        let root = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: dpi,
            dpiY: dpi,
            ..Default::default()
        })?;
        let root_painter = Painter::new(root.cast()?, write)?;
        Ok(Self {
            root,
            root_painter,
            identity: None,
        })
    }

    pub unsafe fn resize(&self, _width: u32, _height: u32, dpi: f32) -> Result<()> {
        self.root.SetDpi(dpi, dpi);
        Ok(())
    }

    pub fn synchronize(&mut self, model: &PanelModel) {
        if self.identity != model.identity {
            self.clear_private();
            self.identity = model.identity;
        }
        self.root_painter
            .bitmaps
            .retain(|id, _| model.records.iter().any(|record| record.id == *id));
        for record in &model.records {
            // SAFETY: this painter is confined to the panel's UI thread. The
            // thumbnail owns a complete BGRA buffer, retained through bitmap
            // creation and in the painter cache alongside the resulting bitmap.
            if let Err(error) = unsafe { self.root_painter.prepare_thumbnail(record) } {
                tracing::debug!("Tray thumbnail upload deferred: {error}");
            }
        }
    }

    pub fn clear_private(&mut self) {
        self.root_painter.bitmaps.clear();
        self.identity = None;
    }

    pub unsafe fn paint(
        &mut self,
        model: &PanelModel,
        controls: &[ControlVisual],
        menu: Option<Menu>,
        dc: HDC,
        client: RECT,
    ) -> Result<()> {
        self.synchronize(model);
        self.root.BindDC(dc, &client)?;
        let palette = Palette::new(model.dark);
        let identity = Matrix3x2 {
            M11: 1.0,
            M22: 1.0,
            ..Default::default()
        };
        self.root.BeginDraw();
        self.root.SetTransform(&identity);
        self.root.Clear(Some(&palette.background));
        let result = (|| -> Result<()> {
            self.paint_root(model, palette)?;
            for control in controls
                .iter()
                .filter(|control| !is_menu_control(control.id))
            {
                let rect = control_rect(control.id);
                self.root.SetTransform(&Matrix3x2 {
                    M31: rect.x,
                    M32: rect.y,
                    ..identity
                });
                self.control(
                    control.id,
                    model,
                    control.focused,
                    control.hovered
                        || control.pressed
                        || menu.is_some_and(|menu| menu.trigger() == control.id),
                    control.enabled,
                )?;
            }
            if let Some(menu) = menu {
                self.root.SetTransform(&identity);
                self.paint_menu(menu, palette);
                for control in controls
                    .iter()
                    .filter(|control| menu.controls().contains(&control.id))
                {
                    let rect = control_rect(control.id);
                    self.root.SetTransform(&Matrix3x2 {
                        M31: rect.x,
                        M32: rect.y,
                        ..identity
                    });
                    self.control(
                        control.id,
                        model,
                        control.focused,
                        control.hovered || control.pressed,
                        control.enabled,
                    )?;
                }
            }
            Ok(())
        })();
        self.root.SetTransform(&identity);
        result.and(self.root.EndDraw(None, None))
    }

    unsafe fn paint_root(&mut self, model: &PanelModel, p: Palette) -> Result<()> {
        let painter = &mut self.root_painter;
        painter.outline(Rect::new(0.5, 0.5, WIDTH - 1.0, HEIGHT - 1.0), 12.0, p.line);
        painter.mark(model.dark, Rect::new(16.0, 13.0, 20.0, 20.0));
        painter.text(
            "CarbonPaper",
            Rect::new(44.0, 8.0, 111.0, 30.0),
            14,
            true,
            p.text,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        if model.lightweight {
            let label = model.labels.get("lightweight");
            let width = painter.measure(label, 11, false)? + 12.0;
            painter.outline(Rect::new(158.0, 13.0, width, 20.0), 4.0, p.line);
            painter.text(
                label,
                Rect::new(164.0, 13.0, width - 12.0, 20.0),
                11,
                false,
                p.muted,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            )?;
        }
        painter.line((0.0, 46.0), (WIDTH, 46.0), p.line, 1.0);
        painter.rounded(Rect::new(16.0, 60.0, 346.0, 42.0), 8.0, p.paper);
        painter.outline(Rect::new(16.0, 60.0, 346.0, 42.0), 8.0, p.line);
        painter.brush.SetColor(&p.muted);
        painter.target.DrawEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: 33.0, y: 79.5 },
                radiusX: 5.0,
                radiusY: 5.0,
            },
            &painter.brush,
            1.2,
            None,
        );
        painter.line((36.5, 83.0), (40.0, 86.5), p.muted, 1.2);
        painter.text(
            model.labels.get("recent"),
            Rect::new(16.0, 116.0, 192.0, 20.0),
            12,
            false,
            p.muted,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        if let Some(record) = model.selected() {
            painter.text(
                &record.date,
                Rect::new(210.0, 116.0, 152.0, 20.0),
                11,
                false,
                p.muted,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?;
        }
        painter.line((16.0, 444.0), (362.0, 444.0), p.line, 1.0);
        let (status, color) = if let Some(message) = &model.message {
            (message.clone(), p.error)
        } else {
            (
                model.status_label(),
                if model.status == "running" {
                    p.success
                } else if model.paused {
                    p.warning
                } else {
                    p.muted
                },
            )
        };
        painter.dot(19.0, 466.0, 3.0, color);
        painter.text(
            &status,
            Rect::new(29.0, 452.0, 333.0, 28.0),
            12,
            false,
            if model.message.is_some() {
                p.error
            } else {
                p.text
            },
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        let fill = if model.capture_controls_enabled() && !model.busy {
            p.accent
        } else {
            blend(p.accent, p.background, 0.55)
        };
        painter.rounded(Rect::new(16.0, 490.0, 346.0, 46.0), 8.0, fill);
        Ok(())
    }

    unsafe fn paint_menu(&self, menu: Menu, p: Palette) {
        let painter = &self.root_painter;
        let r = menu.rect();
        for (offset, opacity) in [(4.0, 0.05), (2.0, 0.1)] {
            painter.rounded(
                Rect::new(
                    r.x - offset,
                    r.y - offset / 2.0,
                    r.w + offset * 2.0,
                    r.h + offset * 2.0,
                ),
                8.0,
                blend(p.background, rgb(0), opacity),
            );
        }
        painter.rounded(r, 8.0, p.surface);
        painter.outline(r, 8.0, p.line);
        for y in match menu {
            Menu::More => &[89.0, 203.0][..],
            Menu::Pause => &[439.0][..],
        } {
            painter.line((r.x + 9.0, *y), (r.x + r.w - 9.0, *y), p.line, 1.0);
        }
    }

    unsafe fn control(
        &mut self,
        id: u16,
        model: &PanelModel,
        focused: bool,
        hovered: bool,
        enabled: bool,
    ) -> Result<()> {
        let p = Palette::new(model.dark);
        let r = control_rect(id);
        let painter = &mut self.root_painter;
        let full = Rect::new(0.0, 0.0, r.w, r.h);
        let text = if enabled { p.text } else { p.muted };
        let center = DWRITE_TEXT_ALIGNMENT_CENTER;
        match id {
            MORE => {
                if hovered && enabled {
                    painter.rounded(full, 6.0, p.hover);
                }
                for x in [9.0, 15.0, 21.0] {
                    painter.dot(x, 15.0, 1.2, p.muted);
                }
            }
            SEARCH_SUBMIT => {
                let fill = if enabled {
                    p.accent
                } else {
                    blend(p.accent, p.paper, 0.55)
                };
                painter.rounded(
                    full,
                    6.0,
                    if hovered && enabled {
                        blend(fill, rgb(0xffffff), 0.08)
                    } else {
                        fill
                    },
                );
                painter.arrow(15.0, 15.0, if enabled { rgb(0xffffff) } else { p.muted });
            }
            PREVIEW => {
                painter.rounded(Rect::new(0.5, 0.5, r.w - 1.0, r.h - 1.0), 8.0, p.paper);
                if let Some(record) = model.selected() {
                    if record.thumbnail.is_some() {
                        painter.thumbnail(record, Rect::new(5.0, 5.0, r.w - 10.0, 163.0))?;
                    } else {
                        painter.text(
                            model.labels.get("previewUnavailable"),
                            Rect::new(15.0, 50.0, r.w - 30.0, 80.0),
                            12,
                            false,
                            p.muted,
                            center,
                        )?;
                    }
                    painter.fill(
                        Rect::new(1.0, 173.0, r.w - 2.0, r.h - 174.0),
                        if hovered && enabled {
                            p.hover
                        } else {
                            p.surface
                        },
                    );
                    painter.line((1.0, 173.0), (r.w - 1.0, 173.0), p.line, 1.0);
                    let caption = Zeroizing::new(format!("{} · {}", record.time, record.app));
                    painter.text(
                        &caption,
                        Rect::new(12.0, 179.0, r.w - 45.0, 20.0),
                        13,
                        true,
                        p.text,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    )?;
                    painter.text(
                        model.labels.get("view"),
                        Rect::new(12.0, 199.0, r.w - 45.0, 17.0),
                        11,
                        false,
                        p.muted,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    )?;
                    painter.arrow(r.w - 19.0, 197.0, p.muted);
                } else {
                    let locked = model.identity.is_none();
                    let key = if !model.ready {
                        "preparing"
                    } else if model.authenticating {
                        "unlocking"
                    } else if model.busy || !model.records_loaded && !locked {
                        "loading"
                    } else if locked {
                        "locked"
                    } else {
                        "empty"
                    };
                    painter.lock(r.w / 2.0, 66.0, p.muted, p.paper);
                    painter.text(
                        model.labels.get(key),
                        Rect::new(12.0, 86.0, r.w - 24.0, 23.0),
                        14,
                        true,
                        p.text,
                        center,
                    )?;
                    let description = model.labels.get(if locked {
                        "lockedDescription"
                    } else {
                        "emptyDescription"
                    });
                    painter.text(
                        description,
                        Rect::new(12.0, 113.0, r.w - 24.0, 22.0),
                        12,
                        false,
                        p.muted,
                        center,
                    )?;
                    if locked && model.ready {
                        let label = model.labels.get(if model.authenticating {
                            "unlocking"
                        } else {
                            "unlock"
                        });
                        let width = painter.measure(label, 12, true)? + 32.0;
                        let button = Rect::new((r.w - width) / 2.0, 148.0, width, 32.0);
                        painter.rounded(
                            button,
                            6.0,
                            if enabled {
                                p.accent
                            } else {
                                blend(p.accent, p.paper, 0.55)
                            },
                        );
                        painter.text(
                            label,
                            button,
                            12,
                            true,
                            if enabled { rgb(0xffffff) } else { p.muted },
                            center,
                        )?;
                    }
                }
                painter.outline(Rect::new(0.5, 0.5, r.w - 1.0, r.h - 1.0), 8.0, p.line);
            }
            RECORD_FIRST..=8 => {
                let index = usize::from(id - RECORD_FIRST);
                let selected = index == model.selected && model.selected().is_some();
                if selected || hovered && enabled {
                    painter.rounded(full, 6.0, if selected { p.accent_surface } else { p.hover });
                }
                if let Some(record) = model.records.get(index) {
                    let latest = index + 1 == model.records.len();
                    painter.text(
                        &record.time,
                        Rect::new(8.0, 6.0, if latest { r.w - 45.0 } else { r.w - 16.0 }, 20.0),
                        12,
                        false,
                        if selected { p.accent } else { text },
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    )?;
                    if latest {
                        painter.text(
                            model.labels.get("latest"),
                            Rect::new(r.w - 39.0, 6.0, 32.0, 20.0),
                            11,
                            false,
                            p.accent,
                            DWRITE_TEXT_ALIGNMENT_TRAILING,
                        )?;
                    }
                    painter.text(
                        &record.app,
                        Rect::new(8.0, 27.0, r.w - 16.0, 17.0),
                        11,
                        false,
                        p.muted,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    )?;
                }
            }
            PAUSE | PAUSE_MORE => {
                let fill = if enabled {
                    p.accent
                } else {
                    blend(p.accent, p.background, 0.55)
                };
                if hovered && enabled {
                    let highlight = blend(fill, rgb(0xffffff), 0.08);
                    painter.rounded(full, 8.0, highlight);
                    let join = if id == PAUSE {
                        Rect::new(r.w - 8.0, 0.0, 8.0, r.h)
                    } else {
                        Rect::new(0.0, 0.0, 8.0, r.h)
                    };
                    painter.fill(join, highlight);
                }
                let foreground = if enabled { rgb(0xffffff) } else { p.muted };
                if id == PAUSE_MORE {
                    painter.line(
                        (0.5, 10.0),
                        (0.5, r.h - 10.0),
                        blend(fill, foreground, 0.25),
                        1.0,
                    );
                    painter.line(
                        (r.w / 2.0 - 4.0, r.h / 2.0 - 2.0),
                        (r.w / 2.0, r.h / 2.0 + 2.0),
                        foreground,
                        1.3,
                    );
                    painter.line(
                        (r.w / 2.0, r.h / 2.0 + 2.0),
                        (r.w / 2.0 + 4.0, r.h / 2.0 - 2.0),
                        foreground,
                        1.3,
                    );
                } else {
                    let label = model.capture_button_label();
                    let width = painter.measure(label, 14, true)?;
                    let start = (r.w - width - 24.0) / 2.0;
                    if model.power_saving || (model.running && !model.paused) {
                        painter.pause(start + 6.0, r.h / 2.0, foreground);
                    } else {
                        painter.play(start + 6.0, r.h / 2.0, foreground);
                    }
                    painter.text(
                        label,
                        Rect::new(start + 24.0, 0.0, width + 1.0, r.h),
                        14,
                        true,
                        foreground,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    )?;
                }
            }
            TIMELINE | EXCLUDE => {
                if hovered && enabled {
                    painter.rounded(full, 5.0, p.hover);
                }
                let foreground = if id == TIMELINE && enabled {
                    p.accent
                } else {
                    p.muted
                };
                let key = if id == TIMELINE {
                    "timeline"
                } else if model.excluded {
                    "recordApp"
                } else {
                    "ignoreApp"
                };
                if id == TIMELINE {
                    painter.clock(8.0, 15.0, foreground);
                } else {
                    painter.brush.SetColor(&foreground);
                    painter.target.DrawEllipse(
                        &D2D1_ELLIPSE {
                            point: D2D_POINT_2F { x: 8.0, y: 15.0 },
                            radiusX: 6.0,
                            radiusY: 3.5,
                        },
                        &painter.brush,
                        1.0,
                        None,
                    );
                    painter.line((2.0, 9.0), (14.0, 21.0), foreground, 1.2);
                }
                painter.text(
                    model.labels.get(key),
                    Rect::new(21.0, 0.0, r.w - 23.0, r.h),
                    12,
                    false,
                    foreground,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?;
            }
            LIGHTWEIGHT | MENU_LOCK | MENU_SETTINGS | MENU_RESTART | MENU_QUIT | MENU_PAUSE_15
            | MENU_PAUSE_30 | MENU_PAUSE_60 | MENU_PAUSE_MANUAL => {
                if hovered && enabled {
                    painter.rounded(full, 5.0, p.hover);
                }
                let foreground = if id == MENU_QUIT && enabled {
                    p.error
                } else {
                    text
                };
                let icon = if id == MENU_QUIT && enabled {
                    p.error
                } else {
                    p.muted
                };
                let key = match id {
                    LIGHTWEIGHT => "lightweight",
                    MENU_LOCK => "lock",
                    MENU_SETTINGS => "settings",
                    MENU_RESTART => "restart",
                    MENU_QUIT => "quit",
                    MENU_PAUSE_15 => "pause15",
                    MENU_PAUSE_30 => "pause30",
                    MENU_PAUSE_60 => "pause60",
                    _ => "pauseManual",
                };
                match id {
                    LIGHTWEIGHT => {
                        painter.outline(Rect::new(9.0, 11.0, 12.0, 12.0), 6.0, icon);
                        painter.line((8.0, 24.0), (21.0, 11.0), icon, 1.0);
                        let x = r.w - 37.0;
                        painter.rounded(
                            Rect::new(x, 9.0, 28.0, 16.0),
                            8.0,
                            if model.lightweight { p.accent } else { p.line },
                        );
                        painter.dot(
                            if model.lightweight { x + 20.0 } else { x + 8.0 },
                            17.0,
                            5.0,
                            if model.lightweight {
                                rgb(0xffffff)
                            } else {
                                p.muted
                            },
                        );
                    }
                    MENU_LOCK => painter.lock(15.0, 17.0, icon, p.surface),
                    MENU_SETTINGS => {
                        for y in [12.0, 21.0] {
                            painter.line((9.0, y), (21.0, y), icon, 1.0);
                        }
                        painter.dot(12.0, 12.0, 2.0, icon);
                        painter.dot(18.0, 21.0, 2.0, icon);
                    }
                    MENU_RESTART => {
                        painter.clock(15.0, 17.0, icon);
                        painter.line((8.0, 9.0), (8.0, 15.0), icon, 1.2);
                        painter.line((8.0, 15.0), (13.0, 15.0), icon, 1.2);
                    }
                    MENU_QUIT => {
                        painter.line((13.0, 10.0), (9.0, 10.0), icon, 1.2);
                        painter.line((9.0, 10.0), (9.0, 24.0), icon, 1.2);
                        painter.line((9.0, 24.0), (13.0, 24.0), icon, 1.2);
                        painter.arrow(18.0, 17.0, icon);
                    }
                    MENU_PAUSE_MANUAL => painter.pause(15.0, 17.0, icon),
                    _ => painter.clock(15.0, 17.0, icon),
                }
                painter.text(
                    model.labels.get(key),
                    Rect::new(
                        32.0,
                        0.0,
                        r.w - if id == LIGHTWEIGHT { 76.0 } else { 40.0 },
                        r.h,
                    ),
                    12,
                    false,
                    foreground,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?;
            }
            _ => {}
        }
        if focused && enabled {
            painter.outline(
                Rect::new(1.5, 1.5, r.w - 3.0, r.h - 3.0),
                5.0,
                if id == PAUSE || id == PAUSE_MORE {
                    blend(p.accent, rgb(0xffffff), 0.5)
                } else {
                    p.accent
                },
            );
        }
        Ok(())
    }
}
