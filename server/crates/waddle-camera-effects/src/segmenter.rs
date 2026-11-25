//! Segments a single newest camera sample on a bounded CPU worker.

use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rten::{op_registry, Model, ModelOptions, RunOptions, ThreadPool};
use rten_tensor::{AsView, Layout, NdTensor, Tensor};

use crate::frame::{rgb_at, Geometry, Plane};

pub const SIDE: usize = 256;
const PIXELS: usize = SIDE * SIDE;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(80);
const MAX_MASK_AGE: Duration = Duration::from_millis(250);

struct Sample {
    image: Vec<f32>,
    geometry: Geometry,
    captured: Instant,
}

struct Mask {
    values: Arc<[u8]>,
    geometry: Geometry,
    captured: Instant,
}

pub struct Segmenter {
    sender: Option<SyncSender<Sample>>,
    worker: Option<JoinHandle<()>>,
    result: Arc<Mutex<Option<Mask>>>,
    last_sample: Option<(Instant, Geometry)>,
}

fn model() -> Option<Arc<Model>> {
    static MODEL: OnceLock<Option<Arc<Model>>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            // Link only the nine ONNX operators used by the bundled graph.
            ModelOptions::with_ops(op_registry!(
                Conv,
                Relu,
                Add,
                HardSwish,
                Sigmoid,
                ReduceMean,
                Mul,
                Resize,
                ConvTranspose
            ))
            .external_data_static(
                "mediapipe_selfie.data",
                rten::include_external_data!("../models/mediapipe_selfie.data"),
            )
            .load_static_slice(include_bytes!("../models/mediapipe_selfie.onnx"))
            .ok()
            .map(Arc::new)
        })
        .clone()
}

pub(crate) fn infer(model: &Model, image: Vec<f32>, pool: &Arc<ThreadPool>) -> Option<Vec<u8>> {
    let input = Tensor::from_data(&[1, 3, SIDE, SIDE], image);
    let options = RunOptions::default().with_thread_pool(Some(Arc::clone(pool)));
    let mask: NdTensor<f32, 4> = model
        .run_one(input.view().into(), Some(options))
        .ok()?
        .try_into()
        .ok()?;
    if mask.shape() != [1, 1, SIDE, SIDE] {
        return None;
    }
    Some(
        mask.iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect(),
    )
}

impl Segmenter {
    pub fn new() -> Option<Self> {
        let model = model()?;
        let (sender, receiver) = mpsc::sync_channel::<Sample>(1);
        let result = Arc::new(Mutex::new(None));
        let output = Arc::clone(&result);
        let worker = thread::Builder::new()
            .name("camera-segmenter".into())
            .spawn(move || {
                let pool = Arc::new(ThreadPool::with_num_threads(2));
                while let Ok(sample) = receiver.recv() {
                    if sample.captured.elapsed() > MAX_MASK_AGE {
                        continue;
                    }
                    let mask = infer(&model, sample.image, &pool).map(|values| Mask {
                        values: values.into(),
                        geometry: sample.geometry,
                        captured: sample.captured,
                    });
                    if let Ok(mut previous) = output.lock() {
                        *previous = mask;
                    }
                }
            })
            .ok()?;
        Some(Self {
            sender: Some(sender),
            worker: Some(worker),
            result,
            last_sample: None,
        })
    }

    pub fn mask(&mut self, geometry: Geometry, planes: &[Plane<'_>; 3]) -> Option<Arc<[u8]>> {
        let now = Instant::now();
        if !self.last_sample.is_some_and(|(time, size)| {
            size == geometry && now.duration_since(time) < SAMPLE_INTERVAL
        }) {
            self.last_sample = Some((now, geometry));
            let square = Geometry {
                width: SIDE,
                height: SIDE,
                rotation: geometry.rotation,
            };
            let mut image = vec![0.; 3 * PIXELS];
            for row in 0..SIDE {
                let y = (2 * row + 1) * geometry.height / (2 * SIDE);
                for col in 0..SIDE {
                    let x = (2 * col + 1) * geometry.width / (2 * SIDE);
                    let (u, v) = square.raw_to_display(col, row);
                    for (channel, val) in rgb_at(planes, x, y).into_iter().enumerate() {
                        image[channel * PIXELS + v * SIDE + u] = f32::from(val) / 255.;
                    }
                }
            }
            if let Some(sender) = &self.sender {
                let _ = sender.try_send(Sample {
                    image,
                    geometry,
                    captured: now,
                });
            }
        }
        let result = self.result.lock().ok()?;
        let valid = result
            .as_ref()
            .filter(|mask| mask.geometry == geometry && mask.captured.elapsed() < MAX_MASK_AGE)?;
        Some(Arc::clone(&valid.values))
    }
}

impl Drop for Segmenter {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rten::ThreadPool;

    use super::{infer, model, PIXELS, SIDE};

    #[test]
    fn selfie_mask_matches_the_original_mediapipe_checkpoint() {
        let portrait = include_bytes!("../tests/data/astronaut.rgb");
        let reference = include_bytes!("../tests/data/astronaut-mask.raw");
        let mut rgb = vec![0.; PIXELS * 3];
        for y in 0..SIDE {
            for x in 0..SIDE {
                let offset = 3 * (64 * (y / 4) + x / 4);
                for ch in 0..3 {
                    rgb[ch * PIXELS + y * SIDE + x] = f32::from(portrait[offset + ch]) / 255.;
                }
            }
        }
        let pool = Arc::new(ThreadPool::with_num_threads(2));
        let mask = infer(&model().unwrap(), rgb, &pool).unwrap();
        for (i, value) in reference.iter().enumerate() {
            let x = 8 * (i % 32) + 4;
            let y = 8 * (i / 32) + 4;
            assert!(mask[y * SIDE + x].abs_diff(*value) <= 2, "at {x},{y}");
        }
    }
}
