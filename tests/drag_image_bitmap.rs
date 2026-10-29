// Exercise the production pixel builder without starting Corral, OLE, GDI, or a Shell target.
// Integration tests deliberately survive the worker's required `--skip dragdrop::` filter.
#[path = "../src/dragdrop/bitmap.rs"]
mod bitmap;

#[test]
fn converts_premultiplied_bgra_to_shell_straight_alpha_without_changing_source() {
    let input = [16, 32, 64, 128, 5, 100, 255, 255, 19, 21, 22, 0];
    let saved = input;
    let out = bitmap::straight_bgra(3, 1, &input).unwrap();
    assert_eq!(out, [32, 64, 128, 128, 5, 100, 255, 255, 0, 0, 0, 0]);
    assert_eq!(input, saved);
}

#[test]
fn shell_premultiplication_recovers_every_valid_channel_and_alpha() {
    for alpha in 1..=255u32 {
        let input: Vec<_> = (0..=alpha)
            .flat_map(|c| [c as u8, c as u8, c as u8, alpha as u8])
            .collect();
        let out = bitmap::straight_bgra(alpha + 1, 1, &input).unwrap();
        for (original, straight) in input.chunks_exact(4).zip(out.chunks_exact(4)) {
            assert_eq!(straight[3], original[3]);
            for channel in 0..3 {
                let recovered = (u32::from(straight[channel]) * alpha + 127) / 255;
                assert_eq!(recovered, u32::from(original[channel]), "alpha={alpha}");
            }
        }
    }
}

#[test]
fn alpha_colour_key_preserves_opaque_black_white_and_magenta() {
    // CLR_NONE is outside the 24-bit COLORREF range: neither black nor any icon colour is keyed.
    assert_eq!(bitmap::COLOR_KEY, 0xFFFF_FFFF);
    assert_ne!(bitmap::COLOR_KEY, 0);
    assert!(bitmap::COLOR_KEY > 0x00FF_FFFF);
    let input = [0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 255, 255];
    assert_eq!(bitmap::straight_bgra(3, 1, &input).unwrap(), input);
}

#[test]
fn synthetic_translucent_icon_remains_coloured_after_shell_multiplication() {
    // A tiny, very translucent red/green/blue icon surrounded by transparent pixels. Passing
    // these premultiplied bytes directly to Shell used to round all its colour channels to zero.
    let input = [0, 0, 0, 0, 0, 0, 1, 1, 0, 1, 0, 1, 1, 0, 0, 1];
    let out = bitmap::straight_bgra(2, 2, &input).unwrap();
    let shell_rgb = |p: &[u8]| {
        p[..3]
            .iter()
            .map(|c| u32::from(*c) * u32::from(p[3]) / 255)
            .sum::<u32>()
    };
    assert!(input.chunks_exact(4).all(|p| shell_rgb(p) == 0));
    assert_eq!(
        out.chunks_exact(4)
            .skip(1)
            .map(shell_rgb)
            .collect::<Vec<_>>(),
        [1, 1, 1]
    );
    assert_eq!(&out[..4], &[0, 0, 0, 0]);
}

#[test]
fn invalid_dimensions_and_lengths_are_rejected_before_allocation() {
    for (w, h, pixels) in [
        (0, 1, &[][..]),
        (1, 0, &[][..]),
        (1, 1, &[0; 3][..]),
        (1, 1, &[0; 5][..]),
        (u32::MAX, u32::MAX, &[][..]),
        (i32::MAX as u32, i32::MAX as u32, &[][..]),
    ] {
        assert!(bitmap::straight_bgra(w, h, pixels).is_none());
    }
}

#[test]
fn low_alpha_and_malformed_channels_saturate_without_wrapping() {
    assert_eq!(
        bitmap::straight_bgra(2, 1, &[1, 1, 1, 1, 255, 128, 0, 2]).unwrap(),
        [255, 255, 255, 1, 255, 255, 0, 2]
    );
}
