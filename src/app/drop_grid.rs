//! Pure desktop placement. Coordinates are physical pixels in one shared coordinate space.
//! Exhaustion returns None: keeping the old placement is safer than stacking icons.
pub fn scaled_spacing(spacing: (i32, i32), source_dpi: u32, target_dpi: u32) -> (i32, i32) {
    let scale = |v: i32| ((i64::from(v.max(1)) * i64::from(target_dpi.max(1))
        + i64::from(source_dpi.max(1)) / 2) / i64::from(source_dpi.max(1))).clamp(1, i64::from(i32::MAX)) as i32;
    (scale(spacing.0), scale(spacing.1))
}

/// The grid phase comes from a current Explorer item on the destination monitor, when available.
pub fn anchor(area: (i32, i32, i32, i32), pitch: (i32, i32), sample: Option<(i32, i32)>) -> (i32, i32) {
    let (x, y) = sample.unwrap_or((area.0, area.1));
    (area.0 + (x - area.0).rem_euclid(pitch.0), area.1 + (y - area.1).rem_euclid(pitch.1))
}

pub fn nearest(wanted: (i32, i32), pitch: (i32, i32), footprint: (i32, i32),
    area: (i32, i32, i32, i32), anchor: (i32, i32), blocked: &[(i32, i32, i32, i32)]) -> Option<(i32, i32)> {
    if pitch.0 <= 0 || pitch.1 <= 0 || footprint.0 <= 0 || footprint.1 <= 0 { return None; }
    let mut best = None;
    let mut distance = i128::MAX;
    let mut x = i64::from(anchor.0);
    while x + i64::from(footprint.0) <= i64::from(area.2) {
        let mut y = i64::from(anchor.1);
        while y + i64::from(footprint.1) <= i64::from(area.3) {
            if x >= i64::from(area.0) && y >= i64::from(area.1) && !blocked.iter().any(|&(l,t,r,b)|
                x < i64::from(r) && x + i64::from(footprint.0) > i64::from(l)
                && y < i64::from(b) && y + i64::from(footprint.1) > i64::from(t)) {
                let dx = i128::from(x) - i128::from(wanted.0);
                let dy = i128::from(y) - i128::from(wanted.1);
                let d = dx * dx + dy * dy;
                if d < distance { distance = d; best = Some((x as i32, y as i32)); }
            }
            y += i64::from(pitch.1);
        }
        x += i64::from(pitch.0);
    }
    best
}
