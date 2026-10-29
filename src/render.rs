//! Off-screen rendering for per-pixel-alpha layered windows (owned by worker W6).
//!
//! A [`Surface`] owns a 32bpp top-down premultiplied BGRA DIB section selected into a memory DC,
//! and a Direct2D DC render target bound to that DC. Choice of target: `CreateDCRenderTarget`
//! (+ `BindDC`) draws straight into the DIB's own pixel memory, so `UpdateLayeredWindow` can take the
//! memory DC as its source with no copy, whereas a WIC-bitmap target would need a lock/copy per
//! frame. The DC target is GDI-compatible, hardware-independent (software rasteriser) and supports
//! `D2D1_ALPHA_MODE_PREMULTIPLIED`. ClearType cannot work on alpha surfaces, so text is forced to
//! grayscale antialiasing.
//!
//! Drawing API: closure based. `surface.draw(|ctx| { ctx.clear(..); ctx.fill_rect(..); Ok(()) })`
//! wraps `BeginDraw`/`EndDraw`; `EndDraw` runs even if the closure fails and its error is returned
//! when the closure itself succeeded. All coordinates are device pixels (the target runs at 96 dpi,
//! so one DIP equals one pixel); colours are straight-alpha [`Rgba`], premultiplied by Direct2D.

#![allow(dead_code)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;

use windows::Win32::Foundation::{COLORREF, GENERIC_READ, GENERIC_WRITE, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, ID2D1GeometrySink, ID2D1PathGeometry, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL,
    D2D1_SWEEP_DIRECTION_CLOCKWISE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};
use windows::core::{Error, HSTRING, Interface, Result, w};

/// Straight (non-premultiplied) 8-bit colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba::new(0, 0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    fn to_d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: f32::from(self.r) / 255.0,
            g: f32::from(self.g) / 255.0,
            b: f32::from(self.b) / 255.0,
            a: f32::from(self.a) / 255.0,
        }
    }
}

/// Floating-point rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl RectF {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self { left, top, right, bottom }
    }

    fn to_d2d(self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.left, top: self.top, right: self.right, bottom: self.bottom }
    }

    fn inset(self, d: f32) -> RectF {
        RectF::new(self.left + d, self.top + d, self.right - d, self.bottom - d)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Text appearance for [`Ctx::draw_text`]. Text is vertically centred in the rect, never wraps and
/// is trimmed with an ellipsis. `shadow` is `(colour, dx, dy)` drawn beneath the text.
#[derive(Clone, Copy, Debug)]
pub struct TextStyle<'a> {
    pub family: &'a str,
    pub size_px: f32,
    /// DirectWrite weight, e.g. 400 normal, 700 bold.
    pub weight: u16,
    pub color: Rgba,
    pub align: Align,
    pub shadow: Option<(Rgba, f32, f32)>,
}

struct Factories {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
}

thread_local! {
    static FACTORIES: RefCell<Option<(ID2D1Factory, IDWriteFactory)>> = const { RefCell::new(None) };
}

/// COM is required for WIC; harmless if the thread is already initialised (S_FALSE / mode mismatch).
fn ensure_com() {
    // SAFETY: plain COM initialisation for the calling thread; the result is deliberately ignored.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

fn factories() -> Result<Factories> {
    FACTORIES.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            ensure_com();
            // SAFETY: factory creation has no preconditions beyond valid type arguments.
            let d2d: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
            let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
            *slot = Some((d2d, dwrite));
        }
        let (d2d, dwrite) = slot.as_ref().cloned().ok_or_else(Error::empty)?;
        Ok(Factories { d2d, dwrite })
    })
}

/// DIB section + memory DC + Direct2D DC render target.
pub struct Surface {
    width: u32,
    height: u32,
    dc: HDC,
    bitmap: HBITMAP,
    old_object: HGDIOBJ,
    bits: *mut c_void,
    target: ID2D1DCRenderTarget,
    factories: Factories,
}

impl Surface {
    pub fn new(width: u32, height: u32) -> Result<Surface> {
        let (width, height) = (width.max(1), height.max(1));
        let factories = factories()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        // SAFETY: valid properties struct.
        let target = unsafe { factories.d2d.CreateDCRenderTarget(&props)? };
        // SAFETY: GDI object creation; every failure path releases what was created so far.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(Error::from_win32());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap);
                    let _ = DeleteDC(dc);
                    return Err(Error::from_win32());
                }
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            let old_object = SelectObject(dc, bitmap);
            let surface = Surface { width, height, dc, bitmap, old_object, bits, target, factories };
            surface.bind()?;
            Ok(surface)
        }
    }

    /// Recreate the backing store at a new size (contents are cleared to transparent).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if width == self.width && height == self.height {
            return Ok(());
        }
        *self = Surface::new(width, height)?;
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Premultiplied BGRA, top-down, `width * 4` bytes per row. Only valid between draws.
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: `bits` points at the DIB section's `width * height * 4` bytes, alive as long as `self`.
        unsafe { std::slice::from_raw_parts(self.bits as *const u8, self.width as usize * self.height as usize * 4) }
    }

    /// Writable premultiplied BGRA pixels (for callers that compose bitmaps themselves, e.g. the
    /// scrollbar's `SourceCopy` fills). Only valid outside a draw.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `pixels`; `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.bits as *mut u8, self.width as usize * self.height as usize * 4) }
    }

    /// Memory DC holding the DIB (for `UpdateLayeredWindow`).
    pub fn dc(&self) -> HDC {
        self.dc
    }

    fn bind(&self) -> Result<()> {
        let rect = RECT { left: 0, top: 0, right: self.width as i32, bottom: self.height as i32 };
        // SAFETY: `dc` is a live memory DC with our DIB selected.
        unsafe { self.target.BindDC(self.dc, &rect) }
    }

    /// Run `f` between `BeginDraw` and `EndDraw`.
    pub fn draw<F: FnOnce(&Ctx) -> Result<()>>(&mut self, f: F) -> Result<()> {
        self.bind()?;
        let ctx = Ctx { rt: &self.target, factories: &self.factories };
        // SAFETY: balanced Begin/End on a bound target.
        unsafe {
            self.target.BeginDraw();
            self.target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        let result = f(&ctx);
        // SAFETY: matches BeginDraw above.
        let end = unsafe { self.target.EndDraw(None, None) };
        result.and(end)
    }

    /// Write the surface as a straight-alpha 32bpp PNG (via WIC).
    pub fn save_png(&self, path: &Path) -> Result<()> {
        ensure_com();
        let mut straight = self.pixels().to_vec();
        for px in straight.chunks_exact_mut(4) {
            let a = u32::from(px[3]);
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let name = HSTRING::from(path.as_os_str());
        // SAFETY: standard WIC encode sequence; all pointers come from live locals.
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = wic.CreateStream()?;
            stream.InitializeFromFilename(&name, GENERIC_WRITE.0)?;
            let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
            let frame = frame.ok_or_else(Error::empty)?;
            frame.Initialize(None)?;
            frame.SetSize(self.width, self.height)?;
            let mut format = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut format)?;
            frame.WritePixels(self.height, self.width * 4, &straight)?;
            frame.Commit()?;
            encoder.Commit()?;
            // Release in dependency order so the file handle is closed before returning.
            drop(frame);
            drop(encoder);
            drop(stream);
        }
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: releases the GDI objects created in `new`; the render target is released by its own Drop
        // (it only holds the DC by value, not a reference to the DIB memory after EndDraw).
        unsafe {
            SelectObject(self.dc, self.old_object);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Drawing context valid inside [`Surface::draw`].
pub struct Ctx<'a> {
    rt: &'a ID2D1DCRenderTarget,
    factories: &'a Factories,
}

impl Ctx<'_> {
    /// Run `f` with drawing clipped to `rect` (axis-aligned, balanced push/pop).
    pub fn with_clip<F: FnOnce(&Self) -> Result<()>>(&self, rect: RectF, f: F) -> Result<()> {
        unsafe { self.rt.PushAxisAlignedClip(&rect.to_d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let result = f(self);
        unsafe { self.rt.PopAxisAlignedClip() };
        result
    }

    fn brush(&self, color: Rgba) -> Result<ID2D1SolidColorBrush> {
        // SAFETY: valid colour pointer; default brush properties.
        unsafe { self.rt.CreateSolidColorBrush(&color.to_d2d(), None) }
    }

    pub fn clear(&self, color: Rgba) {
        // SAFETY: inside Begin/End.
        unsafe { self.rt.Clear(Some(&color.to_d2d())) }
    }

    pub fn fill_rect(&self, rect: RectF, color: Rgba) -> Result<()> {
        let brush = self.brush(color)?;
        // SAFETY: inside Begin/End.
        unsafe { self.rt.FillRectangle(&rect.to_d2d(), &brush) };
        Ok(())
    }

    pub fn fill_rounded_rect(&self, rect: RectF, radius: f32, color: Rgba) -> Result<()> {
        let brush = self.brush(color)?;
        let rr = D2D1_ROUNDED_RECT { rect: rect.to_d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: inside Begin/End.
        unsafe { self.rt.FillRoundedRectangle(&rr, &brush) };
        Ok(())
    }

    /// Group-only contour: use the same geometry-group raster path as the
    /// outer-minus-inner outline. The specialised rounded-rectangle primitive
    /// has different edge coverage from that path at some integer DPI radii.
    pub fn fill_group_rounded_rect(&self, rect: RectF, radius: f32, color: Rgba) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::D2D1_FILL_MODE_ALTERNATE;
        let radius = radius.max(0.0).min((rect.right-rect.left)/2.0).min((rect.bottom-rect.top)/2.0);
        let rr = D2D1_ROUNDED_RECT { rect: rect.to_d2d(), radiusX: radius, radiusY: radius };
        let brush = self.brush(color)?;
        unsafe {
            let outer = self.factories.d2d.CreateRoundedRectangleGeometry(&rr)?;
            let shape = self.factories.d2d.CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE, &[Some(outer.cast()?)])?;
            self.rt.FillGeometry(&shape, &brush, None);
        }
        Ok(())
    }

    /// Stroke lies fully inside `rect` (the geometry is inset by half the width).
    pub fn stroke_rounded_rect(&self, rect: RectF, radius: f32, width: f32, color: Rgba) -> Result<()> {
        let brush = self.brush(color)?;
        let half = width / 2.0;
        let rr = D2D1_ROUNDED_RECT {
            rect: rect.inset(half).to_d2d(),
            radiusX: (radius - half).max(0.0),
            radiusY: (radius - half).max(0.0),
        };
        // SAFETY: inside Begin/End.
        unsafe { self.rt.DrawRoundedRectangle(&rr, &brush, width, None::<&ID2D1StrokeStyle>) };
        Ok(())
    }

    /// A border cut from the exact filled shape. Using outer-minus-inner fills
    /// keeps the outer corner on FillRoundedRectangle's contour at every DPI;
    /// no independent pen centreline or pen-radius convention is involved.
    pub fn fill_rounded_border(&self, rect: RectF, radius: f32, width: f32, color: Rgba) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::D2D1_FILL_MODE_ALTERNATE;
        if width <= 0.0 || rect.right <= rect.left || rect.bottom <= rect.top { return Ok(()) }
        let brush = self.brush(color)?;
        let radius = radius.max(0.0).min((rect.right-rect.left)/2.0).min((rect.bottom-rect.top)/2.0);
        let outer = D2D1_ROUNDED_RECT { rect: rect.to_d2d(), radiusX: radius, radiusY: radius };
        let inner_rect = rect.inset(width);
        if inner_rect.right <= inner_rect.left || inner_rect.bottom <= inner_rect.top {
            return self.fill_rounded_rect(rect, radius, color);
        }
        let inner_radius = (radius-width).max(0.0);
        let inner = D2D1_ROUNDED_RECT { rect: inner_rect.to_d2d(), radiusX: inner_radius, radiusY: inner_radius };
        // SAFETY: both geometries and the brush outlive this synchronous fill.
        unsafe {
            let outer = self.factories.d2d.CreateRoundedRectangleGeometry(&outer)?;
            let inner = self.factories.d2d.CreateRoundedRectangleGeometry(&inner)?;
            let ring = self.factories.d2d.CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE,
                &[Some(outer.cast()?), Some(inner.cast()?)])?;
            self.rt.FillGeometry(&ring, &brush, None);
        }
        Ok(())
    }

    /// Closed path of `rect` with an own radius per corner `[top-left, top-right, bottom-right,
    /// bottom-left]` (corral-painting-spec 2.3: the title strip and the body each own two corners).
    fn corner_path(&self, rect: RectF, radii: [f32; 4]) -> Result<ID2D1PathGeometry> {
        let (w, h) = ((rect.right - rect.left).max(0.0), (rect.bottom - rect.top).max(0.0));
        let half = w.min(h) / 2.0;
        let [tl, tr, br, bl] = radii.map(|r| r.max(0.0).min(half));
        let pt = |x: f32, y: f32| D2D_POINT_2F { x, y };
        // SAFETY: the sink is closed before the geometry is used; all arguments are plain values.
        unsafe {
            let path = self.factories.d2d.CreatePathGeometry()?;
            let sink = path.Open()?;
            let arc = |sink: &ID2D1GeometrySink, r: f32, x: f32, y: f32| {
                if r > 0.0 {
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: pt(x, y),
                        size: D2D_SIZE_F { width: r, height: r },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_SMALL,
                    });
                } else {
                    sink.AddLine(pt(x, y));
                }
            };
            sink.BeginFigure(pt(rect.left + tl, rect.top), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(pt(rect.right - tr, rect.top));
            arc(&sink, tr, rect.right, rect.top + tr);
            sink.AddLine(pt(rect.right, rect.bottom - br));
            arc(&sink, br, rect.right - br, rect.bottom);
            sink.AddLine(pt(rect.left + bl, rect.bottom));
            arc(&sink, bl, rect.left, rect.bottom - bl);
            sink.AddLine(pt(rect.left, rect.top + tl));
            arc(&sink, tl, rect.left + tl, rect.top);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            Ok(path)
        }
    }

    /// Fill `rect` with per-corner radii (anti-aliased path fill).
    pub fn fill_corner_shape(&self, rect: RectF, radii: [f32; 4], color: Rgba) -> Result<()> {
        if rect.right <= rect.left || rect.bottom <= rect.top { return Ok(()) }
        let brush = self.brush(color)?;
        let path = self.corner_path(rect, radii)?;
        // SAFETY: inside Begin/End; geometry and brush outlive the call.
        unsafe { self.rt.FillGeometry(&path, &brush, None) };
        Ok(())
    }

    /// A ring of `width` px cut from the inside of the shape (outer path minus inset path), so it
    /// never exceeds `rect` (corral-painting-spec 5).
    pub fn fill_corner_ring(&self, rect: RectF, radii: [f32; 4], width: f32, color: Rgba) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::D2D1_FILL_MODE_ALTERNATE;
        if width <= 0.0 || rect.right <= rect.left || rect.bottom <= rect.top { return Ok(()) }
        let inner_rect = rect.inset(width);
        if inner_rect.right <= inner_rect.left || inner_rect.bottom <= inner_rect.top {
            return self.fill_corner_shape(rect, radii, color);
        }
        let brush = self.brush(color)?;
        let outer = self.corner_path(rect, radii)?;
        let inner = self.corner_path(inner_rect, radii.map(|r| (r - width).max(0.0)))?;
        // SAFETY: both geometries and the brush outlive this synchronous fill.
        unsafe {
            let ring = self.factories.d2d.CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE,
                &[Some(outer.cast()?), Some(inner.cast()?)])?;
            self.rt.FillGeometry(&ring, &brush, None);
        }
        Ok(())
    }

    /// Straight anti-aliased line with round caps (vector glyph fallback, corral-painting-spec 6).
    pub fn stroke_line(&self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgba) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::{D2D1_CAP_STYLE_ROUND, D2D1_STROKE_STYLE_PROPERTIES, D2D1_DASH_STYLE_SOLID, D2D1_LINE_JOIN_ROUND};
        let brush = self.brush(color)?;
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 1.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0,
        };
        // SAFETY: inside Begin/End; the stroke style outlives the call.
        unsafe {
            let style = self.factories.d2d.CreateStrokeStyle(&props, None)?;
            self.rt.DrawLine(D2D_POINT_2F { x: from.0, y: from.1 }, D2D_POINT_2F { x: to.0, y: to.1 }, &brush, width, &style);
        }
        Ok(())
    }

    /// Vertical gradient from `top` to `bottom` colour.
    pub fn fill_linear_gradient(&self, rect: RectF, top: Rgba, bottom: Rgba) -> Result<()> {
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: top.to_d2d() },
            D2D1_GRADIENT_STOP { position: 1.0, color: bottom.to_d2d() },
        ];
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: rect.left, y: rect.top },
            endPoint: D2D_POINT_2F { x: rect.left, y: rect.bottom },
        };
        // SAFETY: valid stop array / properties; brush used only inside Begin/End.
        unsafe {
            let collection = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let brush = self.rt.CreateLinearGradientBrush(&props, None, &collection)?;
            self.rt.FillRectangle(&rect.to_d2d(), &brush);
        }
        Ok(())
    }

    /// Horizontal multistop gradient in a rounded control track, in device pixels.
    pub fn fill_horizontal_gradient(&self, rect: RectF, stops: &[(f32, Rgba)], radius: f32) -> Result<()> {
        if stops.len() < 2 { return Ok(()); }
        let stops: Vec<_> = stops.iter().map(|(position, color)| D2D1_GRADIENT_STOP {
            position: position.clamp(0.0, 1.0), color: color.to_d2d(),
        }).collect();
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: rect.left, y: rect.top },
            endPoint: D2D_POINT_2F { x: rect.right, y: rect.top },
        };
        unsafe {
            let collection = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let brush = self.rt.CreateLinearGradientBrush(&props, None, &collection)?;
            self.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect.to_d2d(), radiusX: radius, radiusY: radius }, &brush);
        }
        Ok(())
    }

    /// Desktop-icon style label: top-aligned, wrapped (word, then emergency character breaks) into at
    /// most `max_lines` lines, ellipsis on the last line. Returns the height used.
    /// Line height = 1.33 em (Segoe UI ascent+descent+gap). PROVISIONAL vs Explorer's exact metrics.
    pub fn draw_label(&self, text: &str, rect: RectF, style: &TextStyle, max_lines: u32) -> Result<f32> {
        let width = (rect.right - rect.left).max(0.0);
        let line_h = (style.size_px * 1.33).ceil();
        let height = (line_h * max_lines.max(1) as f32).min((rect.bottom - rect.top).max(0.0));
        if text.is_empty() || width <= 0.0 || height <= 0.0 {
            return Ok(0.0);
        }
        let layout = make_layout(&self.factories.dwrite, text, style.family, style.size_px, style.weight, width, height)?;
        unsafe {
            let format = layout.cast::<windows::Win32::Graphics::DirectWrite::IDWriteTextFormat>()?;
            format.SetTextAlignment(match style.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            format.SetParagraphAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            format.SetWordWrapping(windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_EMERGENCY_BREAK)?;
            let sign = self.factories.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            layout.SetTrimming(&trimming, &sign)?;
            let clip = RectF::new(rect.left, rect.top, rect.right, rect.top + height);
            self.rt.PushAxisAlignedClip(&clip.to_d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            if let Some((shadow, dx, dy)) = style.shadow {
                let brush = self.brush(shadow)?;
                self.rt.DrawTextLayout(D2D_POINT_2F { x: rect.left + dx, y: rect.top + dy }, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            let brush = self.brush(style.color)?;
            self.rt.DrawTextLayout(D2D_POINT_2F { x: rect.left, y: rect.top }, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            self.rt.PopAxisAlignedClip();
        }
        Ok(height)
    }

    pub fn draw_text(&self, text: &str, rect: RectF, style: &TextStyle) -> Result<()> {
        self.draw_text_impl(text,rect,style,false,false)
    }
    /// As [`Ctx::draw_text`] with the italic flag (Corral titles, corral-painting-spec 4.4).
    pub fn draw_text_italic(&self, text: &str, rect: RectF, style: &TextStyle, italic: bool) -> Result<()> {
        self.draw_text_impl(text,rect,style,false,italic)
    }
    /// Bounded thread-local DirectWrite layout cache for retained settings surfaces.
    /// Font size and device bounds key DPI/width changes; callers still choose paint colours.
    pub fn draw_text_cached(&self,text:&str,rect:RectF,style:&TextStyle)->Result<()> {
        self.draw_text_impl(text,rect,style,true,false)
    }
    fn draw_text_impl(&self,text:&str,rect:RectF,style:&TextStyle,cached:bool,italic:bool)->Result<()> {
        let width = (rect.right - rect.left).max(0.0);
        let height = (rect.bottom - rect.top).max(0.0);
        if text.is_empty() || width <= 0.0 || height <= 0.0 {
            return Ok(());
        }
        let layout = if cached { cached_layout(&self.factories.dwrite,text,style,width,height)? }
            else { make_layout_styled(&self.factories.dwrite,text,style.family,style.size_px,style.weight,italic,width,height)? };
        // SAFETY: layout is trimmed/aligned before use; drawing is inside Begin/End with a balanced clip.
        unsafe {
            let format = layout.cast::<windows::Win32::Graphics::DirectWrite::IDWriteTextFormat>()?;
            format.SetTextAlignment(match style.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let sign = self.factories.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            layout.SetTrimming(&trimming, &sign)?;

            self.rt.PushAxisAlignedClip(&rect.to_d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            if let Some((shadow, dx, dy)) = style.shadow {
                let brush = self.brush(shadow)?;
                self.rt.DrawTextLayout(
                    D2D_POINT_2F { x: rect.left + dx, y: rect.top + dy },
                    &layout,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            let brush = self.brush(style.color)?;
            self.rt.DrawTextLayout(
                D2D_POINT_2F { x: rect.left, y: rect.top },
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            self.rt.PopAxisAlignedClip();
        }
        Ok(())
    }

    /// Draw a premultiplied BGRA image (`w * h * 4` bytes) scaled into `dest`.
    pub fn draw_bitmap(&self, bgra_premultiplied: &[u8], w: u32, h: u32, dest: RectF, opacity: f32) -> Result<()> {
        if w == 0 || h == 0 || bgra_premultiplied.len() < w as usize * h as usize * 4 {
            return Err(Error::from(windows::Win32::Foundation::E_INVALIDARG));
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        // SAFETY: the source slice was length-checked above; D2D copies it during CreateBitmap.
        unsafe {
            let bitmap = self.rt.CreateBitmap(
                D2D_SIZE_U { width: w, height: h },
                Some(bgra_premultiplied.as_ptr() as *const c_void),
                w * 4,
                &props,
            )?;
            self.rt.DrawBitmap(
                &bitmap,
                Some(&dest.to_d2d()),
                opacity.clamp(0.0, 1.0),
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        Ok(())
    }
}

#[derive(PartialEq,Eq)]
struct TextKey { text:String,family:String,size:u32,weight:u16,width:u32,height:u32 }
thread_local! { static TEXT_LAYOUTS: RefCell<std::collections::VecDeque<(TextKey,IDWriteTextLayout)>> = const { RefCell::new(std::collections::VecDeque::new()) }; }
fn cached_layout(dwrite:&IDWriteFactory,text:&str,style:&TextStyle,width:f32,height:f32)->Result<IDWriteTextLayout> {
    let key=TextKey{text:text.into(),family:style.family.into(),size:style.size_px.to_bits(),weight:style.weight,width:width.to_bits(),height:height.to_bits()};
    TEXT_LAYOUTS.with(|cache|{
        let mut cache=cache.borrow_mut();
        if let Some((_,layout))=cache.iter().find(|(k,_)|*k==key){return Ok(layout.clone());}
        let layout=make_layout(dwrite,text,style.family,style.size_px,style.weight,width,height)?;
        if cache.len()>=512 {cache.pop_front();}
        cache.push_back((key,layout.clone()));Ok(layout)
    })
}

fn make_layout(
    dwrite: &IDWriteFactory,
    text: &str,
    family: &str,
    size_px: f32,
    weight: u16,
    max_w: f32,
    max_h: f32,
) -> Result<IDWriteTextLayout> {
    make_layout_styled(dwrite, text, family, size_px, weight, false, max_w, max_h)
}

#[allow(clippy::too_many_arguments)]
fn make_layout_styled(
    dwrite: &IDWriteFactory,
    text: &str,
    family: &str,
    size_px: f32,
    weight: u16,
    italic: bool,
    max_w: f32,
    max_h: f32,
) -> Result<IDWriteTextLayout> {
    let family = HSTRING::from(family);
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: all strings are NUL-terminated HSTRINGs / slices that outlive the calls.
    unsafe {
        let format = dwrite.CreateTextFormat(
            &family,
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT(i32::from(weight)),
            if italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL },
            DWRITE_FONT_STRETCH_NORMAL,
            size_px,
            w!("en-us"),
        )?;
        dwrite.CreateTextLayout(&wide, &format, max_w, max_h)
    }
}

/// Measure single-line text; returns `(width, height)` in pixels.
pub fn measure_text(text: &str, family: &str, size_px: f32, weight: u16) -> Result<(f32, f32)> {
    measure_text_styled(text, family, size_px, weight, false)
}

/// As [`measure_text`] with the italic flag, so a title is measured with the font it is drawn with.
pub fn measure_text_styled(text: &str, family: &str, size_px: f32, weight: u16, italic: bool) -> Result<(f32, f32)> {
    let f = factories()?;
    let layout = make_layout_styled(&f.dwrite, text, family, size_px, weight, italic, 100_000.0, 100_000.0)?;
    let mut m = DWRITE_TEXT_METRICS::default();
    // SAFETY: valid out pointer.
    unsafe { layout.GetMetrics(&mut m)? };
    Ok((m.widthIncludingTrailingWhitespace, m.height))
}

/// Whether `family` is an installed font family (system collection, case-insensitive).
pub fn font_family_exists(family: &str) -> bool {
    font_face(family, 400).is_some()
}

fn font_face(family: &str, weight: u16) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontFace> {
    use windows::Win32::Foundation::BOOL;
    use windows::core::PCWSTR;
    let f = factories().ok()?;
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: valid out pointer; the collection and everything derived from it are released on return.
    unsafe {
        f.dwrite.GetSystemFontCollection(&mut collection, false).ok()?;
        let collection = collection?;
        let name = HSTRING::from(family);
        let (mut index, mut exists) = (0u32, BOOL(0));
        collection.FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists).ok()?;
        if !exists.as_bool() { return None }
        let font = collection.GetFontFamily(index).ok()?
            .GetFirstMatchingFont(DWRITE_FONT_WEIGHT(i32::from(weight)), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL).ok()?;
        font.CreateFontFace().ok()
    }
}

/// For each code point whether `family` has a glyph for it (glyph index 0 = missing); `None` when the
/// family is not installed (corral-painting-spec 6).
pub fn font_has_glyphs(family: &str, codepoints: &[u32]) -> Option<Vec<bool>> {
    let face = font_face(family, 400)?;
    let mut indices = vec![0u16; codepoints.len()];
    // SAFETY: both buffers hold `codepoints.len()` elements.
    unsafe { face.GetGlyphIndices(codepoints.as_ptr(), codepoints.len() as u32, indices.as_mut_ptr()).ok()? };
    Some(indices.into_iter().map(|i| i != 0).collect())
}

/// `(ascent, descent, line gap)` of `family` in em units (corral-painting-spec 2.2).
pub fn font_line_metrics_em(family: &str, weight: u16) -> Option<(f64, f64, f64)> {
    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_METRICS;
    let face = font_face(family, weight)?;
    let mut m = DWRITE_FONT_METRICS::default();
    // SAFETY: valid out pointer.
    unsafe { face.GetMetrics(&mut m) };
    let em = f64::from(m.designUnitsPerEm);
    (em > 0.0).then(|| (f64::from(m.ascent) / em, f64::from(m.descent) / em, f64::from(m.lineGap) / em))
}

/// Release this thread's Direct2D/DirectWrite factories. Call before the UI thread exits:
/// releasing them from thread-local destructors during process teardown raises in KERNELBASE.
pub fn release_factories() {
    TEXT_LAYOUTS.with(|cache|cache.borrow_mut().clear());
    FACTORIES.with(|cell| {
        cell.borrow_mut().take();
    });
}

/// Push the surface to a layered window with per-pixel alpha and an overall `alpha` (0..=255).
pub fn present_layered(hwnd: HWND, surface: &Surface, screen_pos: (i32, i32), alpha: u8) -> Result<()> {
    let pos = POINT { x: screen_pos.0, y: screen_pos.1 };
    let size = SIZE { cx: surface.width as i32, cy: surface.height as i32 };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: all pointers refer to live locals; the DC is the surface's memory DC.
    unsafe {
        UpdateLayeredWindow(hwnd, HDC::default(), Some(&pos), Some(&size), surface.dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Imaging::WICDecodeMetadataCacheOnDemand;

    fn px(s: &Surface, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * s.width() + x) * 4) as usize;
        s.pixels()[i..i + 4].try_into().unwrap()
    }

    fn near(a: [u8; 4], b: [u8; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
    }

    #[test]
    fn clear_is_premultiplied_bgra() {
        let mut s = Surface::new(8, 8).unwrap();
        s.draw(|c| Ok(c.clear(Rgba::new(255, 0, 0, 128)))).unwrap();
        let p = px(&s, 3, 3);
        assert!(near(p, [0, 0, 128, 128]), "{p:?}");
    }

    #[test]
    fn rounded_rect_corner_transparent_center_opaque() {
        let mut s = Surface::new(40, 40).unwrap();
        s.draw(|c| {
            c.clear(Rgba::TRANSPARENT);
            c.fill_rounded_rect(RectF::new(0.0, 0.0, 40.0, 40.0), 12.0, Rgba::new(10, 200, 30, 255))
        })
        .unwrap();
        assert_eq!(px(&s, 0, 0)[3], 0);
        assert_eq!(px(&s, 20, 20), [30, 200, 10, 255]);
    }

    #[test]
    fn text_draws_only_inside_rect() {
        let mut s = Surface::new(100, 40).unwrap();
        let style = TextStyle {
            family: "Segoe UI",
            size_px: 20.0,
            weight: 400,
            color: Rgba::new(255, 255, 255, 255),
            align: Align::Left,
            shadow: Some((Rgba::new(0, 0, 0, 255), 1.0, 1.0)),
        };
        s.draw(|c| {
            c.clear(Rgba::TRANSPARENT);
            c.draw_text("Hg", RectF::new(20.0, 8.0, 80.0, 32.0), &style)
        })
        .unwrap();
        let (mut inside, mut outside) = (0, 0);
        for y in 0..40 {
            for x in 0..100 {
                if px(&s, x, y)[3] != 0 {
                    if (20..80).contains(&x) && (8..32).contains(&y) { inside += 1 } else { outside += 1 }
                }
            }
        }
        assert!(inside > 20, "inside={inside}");
        assert_eq!(outside, 0);
        let (w, h) = measure_text("Hg", "Segoe UI", 20.0, 400).unwrap();
        assert!(w > 10.0 && h > 10.0);
    }

    #[test]
    fn bitmap_lands_at_destination() {
        let mut s = Surface::new(10, 10).unwrap();
        // Premultiplied BGRA: red, green / blue, white.
        let img: [u8; 16] = [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255];
        s.draw(|c| {
            c.clear(Rgba::TRANSPARENT);
            c.draw_bitmap(&img, 2, 2, RectF::new(4.0, 4.0, 6.0, 6.0), 1.0)
        })
        .unwrap();
        assert!(near(px(&s, 4, 4), [0, 0, 255, 255]), "{:?}", px(&s, 4, 4));
        assert!(near(px(&s, 5, 5), [255, 255, 255, 255]), "{:?}", px(&s, 5, 5));
        assert_eq!(px(&s, 3, 3)[3], 0);
        assert_eq!(px(&s, 6, 6)[3], 0);
    }

    #[test]
    fn png_roundtrip_keeps_size() {
        let mut s = Surface::new(33, 17).unwrap();
        s.draw(|c| c.fill_rect(RectF::new(0.0, 0.0, 33.0, 17.0), Rgba::new(1, 2, 3, 255))).unwrap();
        let path = std::env::temp_dir().join("corral_render_test.png");
        s.save_png(&path).unwrap();
        let name = HSTRING::from(path.as_os_str());
        // SAFETY: standard WIC decode of the file just written.
        let (w, h) = unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let dec = wic
                .CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
                .unwrap();
            let frame = dec.GetFrame(0).unwrap();
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h).unwrap();
            (w, h)
        };
        let _ = std::fs::remove_file(&path);
        assert_eq!((w, h), (33, 17));
    }

    #[test]
    fn resize_changes_size() {
        let mut s = Surface::new(4, 4).unwrap();
        s.resize(9, 7).unwrap();
        assert_eq!((s.width(), s.height()), (9, 7));
        assert_eq!(s.pixels().len(), 9 * 7 * 4);
    }
}
