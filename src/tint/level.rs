//! Working-size images (wallpaper-blur-spec 3 and 6): the unblurred copy S and the blurred copy B of
//! one monitor, composed from a scaled picture and a [`Placement`]. Pure.

use super::placement::Placement;
use std::sync::Arc;

/// An opaque BGRX image at working size: `canvas / k` per axis.
#[derive(Debug)]
pub struct Level {
    pub w: usize,
    pub h: usize,
    /// Divisor of the monitor canvas this image was built at.
    pub k: u32,
    /// BGRX, alpha always 255. A solid level is 1 x 1.
    pub bgrx: Vec<u8>,
    /// `Some(COLORREF)` when the source is a flat colour (blurs to itself).
    pub solid: Option<u32>,
    /// 64-bit content hash (wallpaper-blur-spec 6).
    pub hash: u64,
}

impl Level {
    pub fn new(w: usize, h: usize, k: u32, bgrx: Vec<u8>, solid: Option<u32>) -> Level {
        let hash = hash64(&bgrx);
        Level { w, h, k, bgrx, solid, hash }
    }

    /// A 1 x 1 level of one colour.
    pub fn solid(colorref: u32, k: u32) -> Level {
        Level::new(1, 1, k, vec![(colorref >> 16) as u8, (colorref >> 8) as u8, colorref as u8, 255], Some(colorref))
    }

    /// Bytes of pixel data held.
    #[allow(dead_code)]
    pub fn bytes(&self) -> usize {
        self.bgrx.len()
    }
}

/// Fast 64-bit content hash: eight bytes per step, multiply-rotate mixing.
pub fn hash64(bytes: &[u8]) -> u64 {
    let mut h = 0x9E37_79B9_7F4A_7C15u64 ^ bytes.len() as u64;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        let w = u64::from_le_bytes(c.try_into().expect("8 bytes"));
        h = (h ^ w).wrapping_mul(0xFF51_AFD7_ED55_8CCD).rotate_left(31).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    }
    for &b in chunks.remainder() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01B3);
    }
    h ^ (h >> 33)
}

/// A decoded, already scaled part of a picture: straight-alpha BGRA. `region` is the part of the
/// original picture (pixels `x, y, w, h`) that the pixels show.
pub struct Scaled {
    pub w: usize,
    pub h: usize,
    pub bgra: Vec<u8>,
    pub region: (u32, u32, u32, u32),
}

impl Scaled {
    /// A scaled picture that shows the whole original.
    #[cfg(test)]
    pub fn whole(w: usize, h: usize, bgra: Vec<u8>, original: (u32, u32)) -> Scaled {
        Scaled { w, h, bgra, region: (0, 0, original.0, original.1) }
    }
}

fn colorref_bgr(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Build the working canvas of a monitor from the visible part of a picture scaled by [`decode_plan`]
/// (wallpaper-blur-spec 3.1-3.2): every working pixel maps through the placement to the picture;
/// outside it the background colour shows; transparent picture pixels blend over the background.
///
/// [`decode_plan`]: super::placement::decode_plan
pub fn compose_working(
    placement: &Placement,
    image: (u32, u32),
    scaled: &Scaled,
    background: u32,
    canvas: (i32, i32),
    k: u32,
    size: (usize, usize),
) -> Vec<u8> {
    let bg = colorref_bgr(background);
    let (ww, wh) = size;
    let kf = k.max(1) as f64;
    let (rx, ry) = (scaled.region.0 as f64, scaled.region.1 as f64);
    let (rw, rh) = (scaled.region.2.max(1) as f64, scaled.region.3.max(1) as f64);
    let mut out = vec![255u8; ww * wh * 4];
    let max_x = (canvas.0.max(1) as f64 - 0.5).max(0.0);
    let max_y = (canvas.1.max(1) as f64 - 0.5).max(0.0);
    for y in 0..wh {
        let cy = ((y as f64 + 0.5) * kf).min(max_y);
        for x in 0..ww {
            let cx = ((x as f64 + 0.5) * kf).min(max_x);
            let px = &mut out[(y * ww + x) * 4..][..4];
            let hit = placement.image_at(cx, cy, image).and_then(|(u, v)| {
                if scaled.w == 0 || scaled.h == 0 {
                    return None;
                }
                let sx = (((u - rx) / rw * scaled.w as f64).floor() as isize).clamp(0, scaled.w as isize - 1) as usize;
                let sy = (((v - ry) / rh * scaled.h as f64).floor() as isize).clamp(0, scaled.h as isize - 1) as usize;
                Some(&scaled.bgra[(sy * scaled.w + sx) * 4..][..4])
            });
            match hit {
                Some(s) if s[3] == 255 => px[..3].copy_from_slice(&s[..3]),
                Some(s) => {
                    let a = s[3] as u32;
                    for c in 0..3 {
                        px[c] = ((s[c] as u32 * a + bg[c] as u32 * (255 - a) + 127) / 255) as u8;
                    }
                }
                None => px[..3].copy_from_slice(&bg),
            }
        }
    }
    out
}

/// Mix two levels of equal size by `w_new` (0 = `from`, 1 = `to`); used when a wallpaper change
/// arrives during a cross-fade so the new fade starts from the mix on screen (wallpaper-blur-spec 6).
/// Different sizes cannot be mixed: the newer level is returned.
pub fn blend_levels(from: &Arc<Level>, to: &Arc<Level>, w_new: f32) -> Arc<Level> {
    if w_new >= 1.0 || from.w != to.w || from.h != to.h || from.bgrx.len() != to.bgrx.len() {
        return to.clone();
    }
    if w_new <= 0.0 {
        return from.clone();
    }
    let t = (w_new * 256.0).round() as u32;
    let bytes: Vec<u8> = from
        .bgrx
        .iter()
        .zip(&to.bgrx)
        .map(|(&a, &b)| ((a as u32 * (256 - t) + b as u32 * t + 128) >> 8) as u8)
        .collect();
    let solid = if from.solid.is_some() && from.solid == to.solid { from.solid } else { None };
    Arc::new(Level::new(to.w, to.h, to.k, bytes, solid))
}

#[cfg(test)]
mod tests {
    use super::super::placement::{Mode, RectF64, placement};
    use super::*;

    fn checker(w: usize, h: usize) -> Scaled {
        let bgra = (0..w * h).flat_map(|i| if (i % w + i / w) % 2 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] }).collect();
        Scaled::whole(w, h, bgra, (w as u32, h as u32))
    }

    #[test]
    fn solid_level_is_one_pixel_of_the_colour() {
        let l = Level::solid(0x0030_2010, 4); // COLORREF 0x00BBGGRR
        assert_eq!((l.w, l.h, l.solid), (1, 1, Some(0x0030_2010)));
        assert_eq!(&l.bgrx[..], &[0x30, 0x20, 0x10, 255]);
    }

    #[test]
    fn hash_changes_with_any_byte_and_is_stable() {
        let a = vec![7u8; 1001];
        let mut b = a.clone();
        assert_eq!(hash64(&a), hash64(&b));
        b[1000] = 8;
        assert_ne!(hash64(&a), hash64(&b));
        b[1000] = 7;
        b[500] ^= 1;
        assert_ne!(hash64(&a), hash64(&b));
        assert_ne!(hash64(&[]), hash64(&[0]));
    }

    #[test]
    fn stretch_maps_every_working_pixel_to_the_picture() {
        let canvas = RectF64::new(0.0, 0.0, 16.0, 8.0);
        let p = placement(Mode::Stretch, (8, 4), canvas, canvas);
        let out = compose_working(&p, (8, 4), &checker(8, 4), 0x0000_00ff, (16, 8), 2, (8, 4));
        assert_eq!(out.len(), 8 * 4 * 4);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn a_clipped_picture_maps_through_its_region() {
        // The picture is 8 wide; only columns 4..8 were decoded (all light), so everything is light.
        let canvas = RectF64::new(0.0, 0.0, 4.0, 4.0);
        let p = placement(Mode::Center, (8, 4), canvas, canvas);
        assert_eq!(p.src, RectF64::new(2.0, 0.0, 4.0, 4.0), "the middle 4 columns show");
        let scaled = Scaled { w: 4, h: 4, bgra: [200, 200, 200, 255].repeat(16), region: (2, 0, 4, 4) };
        let out = compose_working(&p, (8, 4), &scaled, 0, (4, 4), 1, (4, 4));
        assert!(out.chunks_exact(4).all(|q| q[0] == 200), "{out:?}");
    }

    #[test]
    fn fit_leaves_background_bars() {
        // 1:1 picture on a 4:1 canvas: bars left and right in the background colour.
        let canvas = RectF64::new(0.0, 0.0, 32.0, 8.0);
        let p = placement(Mode::Fit, (4, 4), canvas, canvas);
        let out = compose_working(&p, (4, 4), &checker(4, 4), 0x00FF_0000, (32, 8), 1, (32, 8)); // blue background
        assert_eq!(&out[..3], &[255, 0, 0], "bar is the background (BGR of 0x00FF0000)");
        let mid = (4 * 32 + 16) * 4;
        assert!(out[mid] == 0 || out[mid] == 255, "picture in the middle");
    }

    #[test]
    fn transparent_picture_blends_over_the_background() {
        let canvas = RectF64::new(0.0, 0.0, 2.0, 2.0);
        let p = placement(Mode::Stretch, (1, 1), canvas, canvas);
        let scaled = Scaled::whole(1, 1, vec![0, 0, 0, 0], (1, 1));
        let out = compose_working(&p, (1, 1), &scaled, 0x0000_ff00, (2, 2), 1, (2, 2));
        assert_eq!(&out[..3], &[0, 255, 0]);
    }

    #[test]
    fn blend_levels_mixes_and_keeps_endpoints() {
        let a = Arc::new(Level::new(1, 1, 1, vec![0, 0, 0, 255], None));
        let b = Arc::new(Level::new(1, 1, 1, vec![200, 100, 50, 255], None));
        assert!(Arc::ptr_eq(&blend_levels(&a, &b, 0.0), &a));
        assert!(Arc::ptr_eq(&blend_levels(&a, &b, 1.0), &b));
        let m = blend_levels(&a, &b, 0.5);
        assert_eq!(&m.bgrx[..3], &[100, 50, 25]);
        let other = Arc::new(Level::new(2, 1, 1, vec![0; 8], None));
        assert!(Arc::ptr_eq(&blend_levels(&a, &other, 0.5), &other), "size mismatch takes the new level");
    }
}
