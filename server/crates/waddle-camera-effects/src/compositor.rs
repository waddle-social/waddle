//! Composites in the camera's I420 color space. Only the small inference tensor uses RGB.

use crate::frame::{Geometry, Plane, PlaneMut};
use crate::segmenter::SIDE;

struct Image {
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    cached: Option<(Geometry, [Vec<u8>; 3])>,
}

pub struct Background {
    image: Option<Image>,
}

impl Background {
    pub fn new(image: Option<(&[u8], usize, usize)>) -> Option<Self> {
        let image = match image {
            None => None,
            Some((rgba, w, h))
                if (1..=1024).contains(&w)
                    && (1..=1024).contains(&h)
                    && rgba.len() == 4 * w * h =>
            {
                Some(Image {
                    rgba: rgba.to_vec(),
                    width: w,
                    height: h,
                    cached: None,
                })
            }
            _ => return None,
        };
        Some(Self { image })
    }

    pub fn compose(
        &mut self,
        g: Geometry,
        input: &[Plane<'_>; 3],
        output: &mut [PlaneMut<'_>; 3],
        mask: Option<&[u8]>,
    ) {
        let Some(mask) = mask else {
            for (channel, dst) in output.iter_mut().enumerate() {
                let (w, h) = g.plane_size(channel);
                for row in dst.data.chunks_mut(dst.stride).take(h) {
                    row[..w].fill(if channel == 0 { 16 } else { 128 });
                }
            }
            return;
        };

        let rotation = Geometry {
            width: SIDE,
            height: SIDE,
            rotation: g.rotation,
        };
        let raw_mask: Vec<u8> = (0..SIDE * SIDE)
            .map(|i| {
                let (x, y) = rotation.raw_to_display(i % SIDE, i / SIDE);
                mask[y * SIDE + x]
            })
            .collect();

        let replacement = self.image.as_mut().map(|image| image.planes(g));
        for (channel, dst) in output.iter_mut().enumerate() {
            let src = &input[channel];
            let (w, h) = g.plane_size(channel);
            let alpha = Grid::new(&raw_mask, SIDE, SIDE, w, h);
            if let Some(background) = &replacement {
                mix(src, dst, w, h, &alpha, |x, y| {
                    background[channel][y * w + x]
                });
            } else {
                let block = if channel == 0 { 24 } else { 12 };
                let pixels = average_blocks(src, w, h, block);
                let blurred = Grid::new(&pixels, w.div_ceil(block), h.div_ceil(block), w, h);
                mix(src, dst, w, h, &alpha, |x, y| blurred.at(x, y));
            }
        }
    }
}

impl Image {
    fn planes(&mut self, g: Geometry) -> &[Vec<u8>; 3] {
        if !self.cached.as_ref().is_some_and(|(size, _)| *size == g) {
            let (dw, dh) = g.display_size();
            let scale = (self.width as f32 / dw as f32).min(self.height as f32 / dh as f32);
            let offset_x = (self.width as f32 - dw as f32 * scale) / 2.;
            let offset_y = (self.height as f32 - dh as f32 * scale) / 2.;
            let mut planes = std::array::from_fn(|ch| {
                let (w, h) = g.plane_size(ch);
                vec![0; w * h]
            });
            let cw = g.width.div_ceil(2);
            for y in 0..g.height {
                for x in 0..g.width {
                    let (dx, dy) = g.raw_to_display(x, y);
                    let u = ((offset_x + (dx as f32 + 0.5) * scale) as usize).min(self.width - 1);
                    let v = ((offset_y + (dy as f32 + 0.5) * scale) as usize).min(self.height - 1);
                    let offset = (v * self.width + u) * 4;
                    let pixel = &self.rgba[offset..offset + 4];
                    let rgb =
                        |channel: usize| i32::from(pixel[channel]) * i32::from(pixel[3]) / 255;
                    let (r, green, b) = (rgb(0), rgb(1), rgb(2));
                    planes[0][y * g.width + x] =
                        (((66 * r + 129 * green + 25 * b + 128) >> 8) + 16) as u8;
                    if x % 2 == 0 && y % 2 == 0 {
                        planes[1][(y / 2) * cw + x / 2] =
                            (((-38 * r - 74 * green + 112 * b + 128) >> 8) + 128) as u8;
                        planes[2][(y / 2) * cw + x / 2] =
                            (((112 * r - 94 * green - 18 * b + 128) >> 8) + 128) as u8;
                    }
                }
            }
            self.cached = Some((g, planes));
        }
        &self.cached.as_ref().expect("image planes were prepared").1
    }
}

fn mix(
    src: &Plane<'_>,
    dst: &mut PlaneMut<'_>,
    w: usize,
    h: usize,
    alpha: &Grid<'_>,
    background: impl Fn(usize, usize) -> u8,
) {
    for y in 0..h {
        let row = &src.data[y * src.stride..][..w];
        let out = &mut dst.data[y * dst.stride..][..w];
        for (x, (fg, value)) in row.iter().zip(out).enumerate() {
            let a = u32::from(alpha.at(x, y));
            *value =
                ((a * u32::from(*fg) + (255 - a) * u32::from(background(x, y)) + 127) / 255) as u8;
        }
    }
}

fn average_blocks(src: &Plane<'_>, w: usize, h: usize, size: usize) -> Vec<u8> {
    let mut values = Vec::with_capacity(w.div_ceil(size) * h.div_ceil(size));
    for y in (0..h).step_by(size) {
        let bottom = h.min(y + size);
        for x in (0..w).step_by(size) {
            let right = w.min(x + size);
            let sum: u32 = (y..bottom)
                .map(|row| {
                    src.data[row * src.stride + x..row * src.stride + right]
                        .iter()
                        .map(|&p| u32::from(p))
                        .sum::<u32>()
                })
                .sum();
            values.push((sum / ((bottom - y) * (right - x)) as u32) as u8);
        }
    }
    values
}

struct Grid<'a> {
    data: &'a [u8],
    width: usize,
    x: Vec<(usize, usize, u32)>,
    y: Vec<(usize, usize, u32)>,
}

impl<'a> Grid<'a> {
    fn new(data: &'a [u8], w: usize, h: usize, ow: usize, oh: usize) -> Self {
        fn axis(src: usize, dst: usize) -> Vec<(usize, usize, u32)> {
            (0..dst)
                .map(|i| {
                    let fixed = ((2 * i + 1) * src * 128 / dst)
                        .saturating_sub(128)
                        .min((src - 1) * 256);
                    (
                        fixed / 256,
                        (fixed / 256 + 1).min(src - 1),
                        (fixed % 256) as u32,
                    )
                })
                .collect()
        }
        Self {
            data,
            width: w,
            x: axis(w, ow),
            y: axis(h, oh),
        }
    }

    fn at(&self, x: usize, y: usize) -> u8 {
        let (x0, x1, fx) = self.x[x];
        let (y0, y1, fy) = self.y[y];
        let sample = |row| {
            u32::from(self.data[row * self.width + x0]) * (256 - fx)
                + u32::from(self.data[row * self.width + x1]) * fx
        };
        ((sample(y0) * (256 - fy) + sample(y1) * fy + 32768) >> 16) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::Background;
    use crate::frame::{Geometry, Plane, PlaneMut};
    use crate::segmenter::SIDE;

    fn render(background: &mut Background, g: Geometry, mask: Option<&[u8]>) -> [Vec<u8>; 3] {
        let src = [
            vec![201; g.width * g.height],
            vec![34; g.width.div_ceil(2) * g.height.div_ceil(2)],
            vec![76; g.width.div_ceil(2) * g.height.div_ceil(2)],
        ];
        let mut out: [Vec<u8>; 3] = std::array::from_fn(|ch| {
            let (w, h) = g.plane_size(ch);
            vec![9; (w + 3) * h]
        });
        let input = std::array::from_fn(|ch| Plane {
            data: &src[ch],
            stride: g.plane_size(ch).0,
        });
        let [y, u, v] = &mut out;
        let mut dst = [
            PlaneMut {
                data: y,
                stride: g.width + 3,
            },
            PlaneMut {
                data: u,
                stride: g.width.div_ceil(2) + 3,
            },
            PlaneMut {
                data: v,
                stride: g.width.div_ceil(2) + 3,
            },
        ];
        background.compose(g, &input, &mut dst, mask);
        out
    }

    #[test]
    fn unavailable_masks_are_neutral_and_padding_is_untouched() {
        let mut bg = Background::new(None).unwrap();
        let result = render(&mut bg, Geometry::new(5, 3, 0).unwrap(), None);
        assert_eq!(&result[0][..8], &[16, 16, 16, 16, 16, 9, 9, 9]);
        assert_eq!(&result[1][..6], &[128, 128, 128, 9, 9, 9]);
    }

    #[test]
    fn image_and_foreground_follow_the_camera_rotation() {
        let white_and_black = [255, 255, 255, 255, 0, 0, 0, 255];
        let mut bg = Background::new(Some((&white_and_black, 2, 1))).unwrap();
        let g = Geometry::new(2, 4, 90).unwrap();
        let mut mask = vec![0; SIDE * SIDE];
        let replacement = render(&mut bg, g, Some(&mask));
        assert_eq!(replacement[0][0], 16);
        assert_eq!(replacement[0][15], 235);
        for row in mask.chunks_mut(SIDE) {
            row[..SIDE / 2].fill(255);
        }
        let composited = render(&mut bg, g, Some(&mask));
        assert_eq!(composited[0][0], 16);
        assert_eq!(composited[0][15], 201);
    }

    #[test]
    fn blur_removes_high_frequency_background_and_keeps_foreground() {
        let g = Geometry::new(48, 12, 0).unwrap();
        let checks: Vec<u8> = (0..48 * 12)
            .map(|i| if i % 2 == 0 { 20 } else { 220 })
            .collect();
        let chroma = vec![128; 24 * 6];
        let input = [
            Plane {
                data: &checks,
                stride: 48,
            },
            Plane {
                data: &chroma,
                stride: 24,
            },
            Plane {
                data: &chroma,
                stride: 24,
            },
        ];
        let mut y = vec![0; 48 * 12];
        let mut u = vec![0; 24 * 6];
        let mut v = u.clone();
        let mut out = [
            PlaneMut {
                data: &mut y,
                stride: 48,
            },
            PlaneMut {
                data: &mut u,
                stride: 24,
            },
            PlaneMut {
                data: &mut v,
                stride: 24,
            },
        ];
        let mut mask = vec![0; SIDE * SIDE];
        for row in mask.chunks_mut(SIDE) {
            row[..SIDE / 2].fill(255);
        }
        Background::new(None)
            .unwrap()
            .compose(g, &input, &mut out, Some(&mask));
        assert_eq!(&y[..8], &checks[..8]);
        assert!(y[26..48].iter().all(|&value| value == 120));
    }
}
