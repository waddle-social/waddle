//! Bounded, borrowed I420 frame planes and display orientation.

pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

pub struct PlaneMut<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub rotation: u32,
}

impl Geometry {
    pub fn new(width: usize, height: usize, rotation: u32) -> Option<Self> {
        ((1..=4096).contains(&width)
            && (1..=4096).contains(&height)
            && width * height <= 4096 * 2160
            && matches!(rotation, 0 | 90 | 180 | 270))
        .then_some(Self {
            width,
            height,
            rotation,
        })
    }

    pub fn plane_size(self, channel: usize) -> (usize, usize) {
        if channel == 0 {
            (self.width, self.height)
        } else {
            (self.width.div_ceil(2), self.height.div_ceil(2))
        }
    }

    pub fn display_size(self) -> (usize, usize) {
        if matches!(self.rotation, 90 | 270) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    pub fn raw_to_display(self, x: usize, y: usize) -> (usize, usize) {
        match self.rotation {
            90 => (self.height - 1 - y, x),
            180 => (self.width - 1 - x, self.height - 1 - y),
            270 => (y, self.width - 1 - x),
            _ => (x, y),
        }
    }
}

pub fn contains_plane(length: usize, stride: usize, width: usize, height: usize) -> bool {
    stride >= width
        && stride
            .checked_mul(height - 1)
            .and_then(|last| last.checked_add(width))
            .is_some_and(|needed| length >= needed)
}

pub fn rgb_at(planes: &[Plane<'_>; 3], x: usize, y: usize) -> [u8; 3] {
    let luma = 298 * (i32::from(planes[0].data[y * planes[0].stride + x]) - 16);
    let u = i32::from(planes[1].data[(y / 2) * planes[1].stride + x / 2]) - 128;
    let v = i32::from(planes[2].data[(y / 2) * planes[2].stride + x / 2]) - 128;
    [luma + 409 * v, luma - 100 * u - 208 * v, luma + 516 * u]
        .map(|c| ((c + 128) >> 8).clamp(0, 255) as u8)
}
