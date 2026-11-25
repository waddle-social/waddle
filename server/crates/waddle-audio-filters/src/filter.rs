use crate::dtln::{self, Dtln};
use crate::rate::Rate;
use nnnoiseless::DenoiseState;
use std::collections::VecDeque;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Bypass,
    Rnnoise,
    Dtln,
}
impl Mode {
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Self::Bypass),
            1 => Some(Self::Rnnoise),
            2 => Some(Self::Dtln),
            _ => None,
        }
    }
}
enum Model {
    Rnnoise(Box<DenoiseState<'static>>),
    Dtln(Box<Dtln>),
}
impl Model {
    fn new(mode: Mode) -> Option<Self> {
        match mode {
            Mode::Rnnoise => Some(Self::Rnnoise(DenoiseState::new())),
            Mode::Dtln => Some(Self::Dtln(Box::new(Dtln::new()?))),
            Mode::Bypass => None,
        }
    }
    fn rate_hop(&self) -> (u32, usize) {
        match self {
            Self::Rnnoise(_) => (48000, nnnoiseless::FRAME_SIZE),
            Self::Dtln(_) => (16000, dtln::HOP),
        }
    }
    fn reset(&mut self) {
        match self {
            Self::Rnnoise(state) => *state = DenoiseState::new(),
            Self::Dtln(state) => state.reset(),
        }
    }
    fn run(&mut self, input: &mut [f32], output: &mut [f32]) -> bool {
        match self {
            Self::Rnnoise(model) => {
                for x in input.iter_mut() {
                    *x *= 32768.0;
                }
                model.process_frame(output, input);
                for x in output.iter_mut() {
                    *x /= 32768.0;
                }
                true
            }
            Self::Dtln(model) => model.process(input, output).is_some(),
        }
    }
}
struct Channel {
    model: Model,
    down: Rate,
    up: Rate,
    hop: usize,
    block: Vec<f32>,
    filtered: Vec<f32>,
    ready: VecDeque<f32>,
}
impl Channel {
    fn new(mode: Mode, rate: u32) -> Option<Self> {
        let model = Model::new(mode)?;
        let (native, hop) = model.rate_hop();
        Some(Self {
            model,
            down: Rate::new(rate, native),
            up: Rate::new(native, rate),
            hop,
            block: Vec::with_capacity(hop),
            filtered: vec![0.0; hop],
            ready: VecDeque::with_capacity(hop * rate as usize / native as usize + 4),
        })
    }
    fn reset(&mut self) {
        self.model.reset();
        self.down.reset();
        self.up.reset();
        self.block.clear();
        self.ready.clear();
    }
    fn sample(&mut self, x: i16) -> Option<i16> {
        let mut ok = true;
        self.down.push(f32::from(x) / 32768.0, |sample| {
            self.block.push(sample);
            if self.block.len() == self.hop {
                ok = self.model.run(&mut self.block, &mut self.filtered)
                    && self.filtered.iter().all(|x| x.is_finite());
                self.block.clear();
                if ok {
                    for &filtered in &self.filtered {
                        self.up.push(filtered, |value| self.ready.push_back(value));
                    }
                }
            }
        });
        ok.then(|| (self.ready.pop_front().unwrap_or(0.0) * 32768.0).round() as i16)
    }
}

pub struct AudioFilter {
    channels: usize,
    lanes: Vec<Channel>,
    staging: Vec<i16>,
}
impl AudioFilter {
    pub fn new(mode: Mode, sample_rate: u32, channels: u32) -> Option<Self> {
        if !(8_000..=96_000).contains(&sample_rate) || !matches!(channels, 1 | 2) {
            return None;
        }
        let channels = channels as usize;
        let lanes = if mode == Mode::Bypass {
            Vec::new()
        } else {
            (0..channels)
                .map(|_| Channel::new(mode, sample_rate))
                .collect::<Option<Vec<_>>>()?
        };
        Some(Self {
            channels,
            lanes,
            staging: vec![0; sample_rate as usize * channels / 10],
        })
    }
    pub fn accepts(&self, count: usize) -> bool {
        count > 0 && count <= self.staging.len() && count.is_multiple_of(self.channels)
    }
    pub fn reset(&mut self) {
        for lane in &mut self.lanes {
            lane.reset();
        }
        self.staging.fill(0);
    }
    pub fn process(&mut self, pcm: &mut [i16]) -> bool {
        if !self.accepts(pcm.len()) {
            return false;
        }
        if self.lanes.is_empty() {
            return true;
        }
        for (i, &sample) in pcm.iter().enumerate() {
            let Some(value) = self.lanes[i % self.channels].sample(sample) else {
                self.reset();
                return false;
            };
            self.staging[i] = value;
        }
        pcm.copy_from_slice(&self.staging[..pcm.len()]);
        true
    }
}
