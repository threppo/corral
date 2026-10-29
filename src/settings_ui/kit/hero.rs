//! Embedded Corral mark for the About hero; never reads the installed product.
use crate::render::{Ctx, RectF};
use windows::{
    Win32::{Graphics::Imaging::*, System::Com::*},
    core::Result,
};
const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/png/corral-256.png");

pub fn draw(ctx: &Ctx, rect: RectF) -> Result<()> {
    thread_local! {static ICON:std::cell::RefCell<Option<(Vec<u8>, u32, u32)>>=const{std::cell::RefCell::new(None)};}
    ICON.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let mut png = ICON_PNG.to_vec();
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let wic: IWICImagingFactory =
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                let stream = wic.CreateStream()?;
                stream.InitializeFromMemory(&mut png)?;
                let decoder = wic.CreateDecoderFromStream(
                    &stream,
                    std::ptr::null(),
                    WICDecodeMetadataCacheOnLoad,
                )?;
                let frame = decoder.GetFrame(0)?;
                let (mut width, mut height) = (0, 0);
                frame.GetSize(&mut width, &mut height)?;
                let converter = wic.CreateFormatConverter()?;
                converter.Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )?;
                let mut data = vec![0; width as usize * height as usize * 4];
                converter.CopyPixels(std::ptr::null(), width * 4, &mut data)?;
                *slot = Some((data, width, height));
            }
        }
        let (pixels, width, height) = slot.as_ref().unwrap();
        ctx.draw_bitmap(pixels, *width, *height, rect, 1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn about_hero_source_covers_the_supported_dpis_without_upscaling() {
        assert_eq!(&ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&ICON_PNG[12..16], b"IHDR");
        let width = u32::from_be_bytes(ICON_PNG[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(ICON_PNG[20..24].try_into().unwrap());
        for dpi in [96, 120, 144, 240] {
            let destination_pixels = 64 * dpi / 96;
            assert!(
                width >= destination_pixels && height >= destination_pixels,
                "About's 64-DIP hero needs {destination_pixels}px at {dpi} DPI, source is {width}x{height}"
            );
        }
    }
}
