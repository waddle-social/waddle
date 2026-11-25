//! On-device camera effects. Each camera track owns its worker, input and foreground mask.

mod compositor;
mod frame;
mod segmenter;

pub use frame::{contains_plane, Geometry, Plane, PlaneMut};

use compositor::Background;
use segmenter::Segmenter;

pub struct CameraEffect {
    segmenter: Segmenter,
    background: Background,
}

impl CameraEffect {
    pub fn new(image: Option<(&[u8], usize, usize)>) -> Option<Self> {
        Some(Self {
            background: Background::new(image)?,
            segmenter: Segmenter::new()?,
        })
    }

    pub fn process(
        &mut self,
        geometry: Geometry,
        input: &[Plane<'_>; 3],
        output: &mut [PlaneMut<'_>; 3],
    ) -> bool {
        for (channel, (src, dst)) in input.iter().zip(output.iter()).enumerate() {
            let (width, height) = geometry.plane_size(channel);
            if !contains_plane(src.data.len(), src.stride, width, height)
                || !contains_plane(dst.data.len(), dst.stride, width, height)
            {
                return false;
            }
        }
        let mask = self.segmenter.mask(geometry, input);
        self.background
            .compose(geometry, input, output, mask.as_deref());
        true
    }
}
