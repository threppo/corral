//! Pixel preparation for IDragSourceHelper::InitializeFromBitmap, without GDI or Shell calls.

/// CLR_NONE: use per-pixel alpha, never key out black (or a colour in the icon).
pub(crate) const COLOR_KEY: u32 = 0xFFFF_FFFF;

/// (API documentation) InitializeFromBitmap multiplies RGB by alpha itself:
/// https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idragsourcehelper-initializefrombitmap
/// Convert the premultiplied render surface to straight BGRA exactly once at that boundary.
/// Round to nearest, keep alpha, canonicalize transparent pixels, and saturate malformed channels.
/// The input is borrowed: the source surface must remain suitable for UpdateLayeredWindow.
pub(crate) fn straight_bgra(width: u32, height: u32, pixels: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return None;
    }
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if pixels.len() != len {
        return None;
    }
    let mut out = pixels.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel.fill(0);
        } else {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Some(out)
}
