//! Thumb position and stored value are intentionally separate; no quantized thumb during a drag.
#[derive(Clone, Debug)]
pub enum SliderScale {
    Continuous { min: i32, max: i32 },
    Steps(Vec<i32>),
}
#[derive(Clone, Debug)]
pub struct SliderModel {
    pub scale: SliderScale,
    pub fraction: f32,
    pub value: i32,
    pub dragging: bool,
}
impl SliderModel {
    pub fn new(scale: SliderScale, value: i32) -> Self {
        let fraction = match &scale {
            SliderScale::Continuous { min, max } => {
                if max > min {
                    (value - *min) as f32 / (*max - *min) as f32
                } else {
                    0.
                }
            }
            SliderScale::Steps(v) => {
                let i = nearest(v, value);
                if v.len() > 1 {
                    i as f32 / (v.len() - 1) as f32
                } else {
                    0.
                }
            }
        };
        Self {
            scale,
            fraction: fraction.clamp(0., 1.),
            value,
            dragging: false,
        }
    }
    pub fn drag(&mut self, x: f32, start: f32, end: f32) -> i32 {
        self.dragging = true;
        self.fraction = if end > start {
            ((x - start) / (end - start)).clamp(0., 1.)
        } else {
            0.
        };
        self.value = self.rounded_value();
        self.value
    }
    pub fn rounded_value(&self) -> i32 {
        match &self.scale {
            SliderScale::Continuous { min, max } => {
                (*min as f32 + self.fraction * (*max - *min) as f32).round() as i32
            }
            SliderScale::Steps(v) => v
                .get((self.fraction * (v.len().saturating_sub(1)) as f32).round() as usize)
                .copied()
                .unwrap_or(0),
        }
    }
    pub fn release(&mut self) {
        self.dragging = false;
        if let SliderScale::Steps(v) = &self.scale {
            self.fraction = if v.len() > 1 {
                nearest(v, self.value) as f32 / (v.len() - 1) as f32
            } else {
                0.
            };
        }
    }
    pub fn thumb(&self, start: f32, end: f32) -> f32 {
        start + self.fraction * (end - start)
    }
    pub fn step(&mut self, delta: i32, page: bool) -> i32 {
        match &self.scale {
            SliderScale::Steps(v) => {
                let i = (nearest(v, self.value) as i32 + delta)
                    .clamp(0, v.len().saturating_sub(1) as i32);
                self.value = v.get(i as usize).copied().unwrap_or(0);
            }
            SliderScale::Continuous { min, max } => {
                let step =
                    (((*max - *min) as f32 * if page { 0.10 } else { 0.01 }).round() as i32).max(1);
                self.value = (self.value + delta * step).clamp(*min, *max);
            }
        }
        let next = Self::new(self.scale.clone(), self.value);
        self.fraction = next.fraction;
        self.value
    }
}
pub fn nearest(values: &[i32], value: i32) -> usize {
    values
        .iter()
        .enumerate()
        .min_by_key(|(_, v)| (**v as i64 - value as i64).abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumb_moves_when_rounded_value_does_not() {
        let mut s = SliderModel::new(SliderScale::Continuous { min: 0, max: 100 }, 0);
        s.drag(100.10, 0., 220.);
        let v = s.value;
        let x = s.thumb(0., 220.);
        s.drag(100.20, 0., 220.);
        assert_eq!(v, s.value);
        assert!(s.thumb(0., 220.) > x);
    }
    #[test]
    fn step_name_rounds_but_thumb_only_snaps_on_release() {
        let mut s = SliderModel::new(SliderScale::Steps(vec![0, 15, 30, 60, 120]), 20);
        assert_eq!(s.value, 20);
        s.drag(0.39, 0., 1.);
        assert_eq!(s.value, 30);
        assert_eq!(s.fraction, 0.39);
        s.release();
        assert_eq!(s.fraction, 0.5);
    }
    #[test]
    fn keyboard_percent_and_step_are_distinct() {
        let mut a = SliderModel::new(SliderScale::Continuous { min: 0, max: 2000 }, 1000);
        assert_eq!(a.step(1, false), 1020);
        assert_eq!(a.step(-1, true), 820);
        let mut b = SliderModel::new(SliderScale::Steps(vec![0, 15, 30, 60, 120]), 30);
        assert_eq!(b.step(1, false), 60);
    }
}
