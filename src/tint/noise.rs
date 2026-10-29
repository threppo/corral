//! Generated noise tile that hides banding in the blurred backdrop (wallpaper-blur-spec 5). No file,
//! no third-party asset: the same bytes on every run.

use std::sync::OnceLock;

/// Tile edge in texels, drawn 1:1 in device pixels (wallpaper-blur-spec 5).
pub const TILE: usize = 128;
/// Largest texel alpha (of 255).
pub const MAX_ALPHA: f32 = 6.0;
const SEED: u32 = 0x5EED_0001;

/// Murmur3 finaliser (wallpaper-blur-spec 5).
pub fn fmix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// Triangular noise value of texel `i`, mean 0, in (-1, 1).
pub fn texel_value(i: u32) -> f64 {
    let u1 = fmix32(SEED.wrapping_add(i.wrapping_mul(2))) as f64 / 4_294_967_296.0;
    let u2 = fmix32(SEED.wrapping_add(i.wrapping_mul(2)).wrapping_add(1)) as f64 / 4_294_967_296.0;
    u1 + u2 - 1.0
}

/// A noise texel: white or black with a small alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texel {
    pub white: bool,
    pub alpha: u8,
}

/// Texel `i` (`n >= 0` white with alpha `round(6 n)`, else black with alpha `round(-6 n)`).
pub fn texel(i: u32) -> Texel {
    let n = texel_value(i);
    Texel { white: n >= 0.0, alpha: (n.abs() * MAX_ALPHA as f64).round() as u8 }
}

/// The whole tile as premultiplied BGRA (wallpaper-blur-spec 5).
#[cfg(test)]
pub fn build_tile() -> Vec<u8> {
    let mut out = Vec::with_capacity(TILE * TILE * 4);
    for i in 0..(TILE * TILE) as u32 {
        let t = texel(i);
        let c = if t.white { t.alpha } else { 0 };
        out.extend_from_slice(&[c, c, c, t.alpha]);
    }
    out
}

/// The tile's texels, built once; the drawing reads them instead of hashing per pixel.
fn texels() -> &'static [Texel] {
    static TEXELS: OnceLock<Vec<Texel>> = OnceLock::new();
    TEXELS.get_or_init(|| (0..(TILE * TILE) as u32).map(texel).collect())
}

/// Tile texel under screen pixel `(x, y)` for a monitor whose top-left is `(mon_x, mon_y)`:
/// tiled from the monitor, never from the Corral, so the pattern does not crawl while a Corral moves
/// (wallpaper-blur-spec 5).
#[cfg(test)]
pub fn texel_at(x: i32, y: i32, mon_x: i32, mon_y: i32) -> Texel {
    texels()[texel_index(x, y, mon_x, mon_y)]
}

/// Index into [`table`] for screen pixel `(x, y)`; the tile edge is a power of two, so wrapping
/// subtraction and a mask equal the Euclidean remainder for negative origins too.
pub fn texel_index(x: i32, y: i32, mon_x: i32, mon_y: i32) -> usize {
    let tx = x.wrapping_sub(mon_x) as u32 as usize & (TILE - 1);
    let ty = y.wrapping_sub(mon_y) as u32 as usize & (TILE - 1);
    ty * TILE + tx
}

/// The tile's texels for [`texel_index`] (row-major, `TILE x TILE`).
pub fn table() -> &'static [Texel] {
    texels()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a of the premultiplied tile bytes; changing the generator changes the tile everywhere.
    const FINGERPRINT: u64 = 0x271e_494d_d232_b32d;

    #[test]
    fn tile_is_byte_identical_across_runs() {
        assert_eq!(build_tile(), build_tile());
        assert_eq!(build_tile().len(), TILE * TILE * 4);
        // Fixed fingerprint: the generator never changes silently.
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for b in build_tile() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        eprintln!("noise tile fingerprint {h:#018x}");
        assert_eq!(h, FINGERPRINT);
    }

    #[test]
    fn statistics_match_the_triangular_distribution() {
        let n: Vec<f64> = (0..(TILE * TILE) as u32).map(texel_value).collect();
        let mean = n.iter().sum::<f64>() / n.len() as f64;
        let var = n.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n.len() as f64;
        assert!(mean.abs() < 0.02, "mean {mean}");
        assert!((var - 1.0 / 6.0).abs() < 0.05 / 6.0, "variance {var}");
        let max = build_tile().chunks_exact(4).map(|p| p[3]).max().unwrap();
        assert_eq!(max, 6);
    }

    #[test]
    fn premultiplied_texels_are_consistent() {
        for p in build_tile().chunks_exact(4) {
            assert!(p[0] == p[1] && p[1] == p[2]);
            assert!(p[0] == 0 || p[0] == p[3], "white premultiplied = alpha, black = 0");
        }
    }

    #[test]
    fn no_two_rows_are_identical() {
        let t = build_tile();
        let rows: Vec<&[u8]> = t.chunks_exact(TILE * 4).collect();
        for a in 0..rows.len() {
            for b in a + 1..rows.len() {
                assert_ne!(rows[a], rows[b], "rows {a} and {b}");
            }
        }
    }

    #[test]
    fn wrap_neighbours_are_uncorrelated() {
        let v: Vec<f64> = (0..(TILE * TILE) as u32).map(texel_value).collect();
        let corr = |dx: usize, dy: usize| {
            let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
            for y in 0..TILE {
                for x in 0..TILE {
                    let a = v[y * TILE + x];
                    let b = v[((y + dy) % TILE) * TILE + (x + dx) % TILE];
                    sxy += a * b;
                    sxx += a * a;
                    syy += b * b;
                }
            }
            sxy / (sxx * syy).sqrt()
        };
        // Across the right edge, across the bottom edge, and inside.
        for (dx, dy) in [(1, 0), (0, 1), (1, 1), (TILE - 1, 0), (0, TILE - 1)] {
            let r = corr(dx, dy);
            assert!(r.abs() < 0.05, "offset ({dx},{dy}): r {r}");
        }
    }

    #[test]
    fn screen_pixel_noise_depends_on_the_monitor_not_the_corral() {
        let tile = build_tile();
        for (mx, my) in [(0, 0), (-1920, -200), (1920, 37)] {
            for (x, y) in [(5, 9), (-1900, -150), (2000, 400), (-3, -3)] {
                let t = texel_at(x, y, mx, my);
                let tx = (x - mx).rem_euclid(128) as usize;
                let ty = (y - my).rem_euclid(128) as usize;
                let p = &tile[(ty * TILE + tx) * 4..][..4];
                assert_eq!(p[3], t.alpha);
                assert_eq!(p[0], if t.white { t.alpha } else { 0 });
            }
        }
    }

    #[test]
    fn budget_smoke_tile_build() {
        let started = std::time::Instant::now();
        let t = build_tile();
        eprintln!("noise tile build: {:?}", started.elapsed());
        assert!(!t.is_empty());
    }
}
