//! Two-stage DTLN streaming inference with a 512-sample window and 128-sample overlap-add hop.
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rten::{op_registry, Model, ModelOptions, RunOptions, ThreadPool};
use rten_tensor::{AsView, Layout, NdTensor};
use std::sync::{Arc, OnceLock};
const FFT: usize = 512;
const BINS: usize = FFT / 2 + 1;
pub const HOP: usize = 128;
struct Weights {
    mask: Model,
    decoder: Model,
}
fn weights() -> Option<Arc<Weights>> {
    static WEIGHTS: OnceLock<Option<Arc<Weights>>> = OnceLock::new();
    WEIGHTS
        .get_or_init(|| {
            let load = |source: &'static [u8]| {
                ModelOptions::with_ops(op_registry!(
                    Squeeze, Unsqueeze, Transpose, Slice, Concat, LSTM, MatMul, Add, Sigmoid, Mul,
                    Split, Tanh, Conv, ReduceMean, Sub, Reshape, Sqrt, Div, Pad
                ))
                .load_static_slice(source)
                .ok()
            };
            Some(Arc::new(Weights {
                mask: load(include_bytes!("../models/dtln_mask.onnx"))?,
                decoder: load(include_bytes!("../models/dtln_decoder.onnx"))?,
            }))
        })
        .clone()
}
pub struct Dtln {
    weights: Arc<Weights>,
    pool: Arc<ThreadPool>,
    mask_state: NdTensor<f32, 4>,
    decoder_state: NdTensor<f32, 4>,
    window: [f32; FFT],
    overlap: [f32; FFT],
    block: [f32; FFT],
    spectrum: Vec<Complex32>,
    fft_scratch: Vec<Complex32>,
    ifft_scratch: Vec<Complex32>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
}
impl Dtln {
    pub fn new() -> Option<Self> {
        let weights = weights()?;
        let mut planner = RealFftPlanner::new();
        let fft = planner.plan_fft_forward(FFT);
        let ifft = planner.plan_fft_inverse(FFT);
        let mut filter = Self {
            weights,
            pool: Arc::new(ThreadPool::with_num_threads(1)),
            mask_state: NdTensor::zeros([1, 2, 128, 2]),
            decoder_state: NdTensor::zeros([1, 2, 128, 2]),
            window: [0.0; FFT],
            overlap: [0.0; FFT],
            block: [0.0; FFT],
            spectrum: fft.make_output_vec(),
            fft_scratch: fft.make_scratch_vec(),
            ifft_scratch: ifft.make_scratch_vec(),
            fft,
            ifft,
        };
        // Prepare both graphs before the capture thread starts delivering samples.
        filter.process(&[0.0; HOP], &mut [0.0; HOP])?;
        filter.reset();
        Some(filter)
    }
    pub fn reset(&mut self) {
        self.mask_state.fill(0.0);
        self.decoder_state.fill(0.0);
        self.window.fill(0.0);
        self.overlap.fill(0.0);
    }
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Option<()> {
        self.window.copy_within(HOP.., 0);
        self.window[FFT - HOP..].copy_from_slice(input);
        self.block = self.window;
        self.fft
            .process_with_scratch(&mut self.block, &mut self.spectrum, &mut self.fft_scratch)
            .ok()?;
        let magnitude = self.spectrum.iter().map(|c| c.norm()).collect();
        let mask = stage(
            &self.weights.mask,
            &self.pool,
            &mut self.mask_state,
            magnitude,
            BINS,
        )?;
        for (c, &m) in self.spectrum.iter_mut().zip(mask.iter()) {
            *c *= m;
        }
        self.ifft
            .process_with_scratch(&mut self.spectrum, &mut self.block, &mut self.ifft_scratch)
            .ok()?;
        let estimated = self.block.iter().map(|x| *x / FFT as f32).collect();
        let decoded = stage(
            &self.weights.decoder,
            &self.pool,
            &mut self.decoder_state,
            estimated,
            FFT,
        )?;
        self.overlap.copy_within(HOP.., 0);
        self.overlap[FFT - HOP..].fill(0.0);
        for (x, &y) in self.overlap.iter_mut().zip(decoded.iter()) {
            *x += y;
        }
        output.copy_from_slice(&self.overlap[..HOP]);
        Some(())
    }
}
fn stage(
    model: &Model,
    pool: &Arc<ThreadPool>,
    memory: &mut NdTensor<f32, 4>,
    samples: Vec<f32>,
    size: usize,
) -> Option<NdTensor<f32, 3>> {
    let [data_id, state_id] = *model.input_ids() else {
        return None;
    };
    let [out_id, next_id] = *model.output_ids() else {
        return None;
    };
    let data = NdTensor::from_data([1, 1, size], samples);
    let opts = RunOptions::default().with_thread_pool(Some(pool.clone()));
    let [out, state] = model
        .run_n(
            vec![
                (data_id, data.view().into()),
                (state_id, memory.view().into()),
            ],
            [out_id, next_id],
            Some(opts),
        )
        .ok()?;
    let next: NdTensor<f32, 4> = state.try_into().ok()?;
    let block: NdTensor<f32, 3> = out.try_into().ok()?;
    if next.shape() != [1, 2, 128, 2] || block.shape() != [1, 1, size] {
        return None;
    }
    *memory = next;
    Some(block)
}
