//! Causal polyphase sample-rate conversion around fixed-rate speech models.
use std::f64::consts::PI;
const TAPS: usize = 48;
const PHASES: usize = 256;

pub struct Rate {
    source: u32,
    target: u32,
    phase: u32,
    index: usize,
    history: [f32; TAPS],
    bank: Option<Vec<[f32; TAPS]>>,
}
impl Rate {
    pub fn new(source: u32, target: u32) -> Self {
        let bank = (source != target).then(|| {
            let cutoff = 0.47 * (f64::from(target) / f64::from(source)).min(1.0);
            (0..=PHASES)
                .map(|p| {
                    let mut row = [0.0; TAPS];
                    let mut sum = 0.0;
                    for (i, c) in row.iter_mut().enumerate() {
                        let x = i as f64 - (TAPS as f64 - 1.0) * 0.5 - p as f64 / PHASES as f64;
                        let angle = 2.0 * PI * cutoff * x;
                        let window = 0.42 - 0.5 * (2.0 * PI * i as f64 / (TAPS - 1) as f64).cos()
                            + 0.08 * (4.0 * PI * i as f64 / (TAPS - 1) as f64).cos();
                        *c = (2.0
                            * cutoff
                            * if angle.abs() < 1e-12 {
                                1.0
                            } else {
                                angle.sin() / angle
                            }
                            * window) as f32;
                        sum += *c;
                    }
                    for c in &mut row {
                        *c /= sum;
                    }
                    row
                })
                .collect()
        });
        Self {
            source,
            target,
            phase: 0,
            index: 0,
            history: [0.0; TAPS],
            bank,
        }
    }
    pub fn reset(&mut self) {
        self.phase = 0;
        self.index = 0;
        self.history.fill(0.0);
    }
    pub fn push(&mut self, sample: f32, mut emit: impl FnMut(f32)) {
        let Some(bank) = &self.bank else {
            emit(sample);
            return;
        };
        self.history[self.index] = sample;
        self.phase += self.target;
        while self.phase >= self.source {
            self.phase -= self.source;
            let p =
                (self.phase as usize * PHASES + self.target as usize / 2) / self.target as usize;
            let coeff = &bank[p];
            let split = self.index + 1;
            let value = self.history[..split]
                .iter()
                .rev()
                .chain(self.history[split..].iter().rev())
                .zip(coeff)
                .map(|(x, c)| x * c)
                .sum();
            emit(value);
        }
        self.index = (self.index + 1) % TAPS;
    }
}

#[cfg(test)]
mod tests {
    use super::Rate;
    use std::f32::consts::TAU;

    fn level_after_downsampling(hz: f32) -> f32 {
        let mut rate = Rate::new(48_000, 16_000);
        let mut filtered = Vec::new();
        for n in 0..4800 {
            rate.push((TAU * hz * n as f32 / 48_000.0).sin(), |x| filtered.push(x));
        }
        assert_eq!(filtered.len(), 1600);
        let settled = &filtered[200..];
        (settled.iter().map(|v| v * v).sum::<f32>() / settled.len() as f32).sqrt()
    }

    #[test]
    fn capture_downsampling_retains_speech_and_suppresses_aliases() {
        assert!((0.69..0.72).contains(&level_after_downsampling(1_000.0)));
        assert!(level_after_downsampling(12_000.0) < 0.002);
    }
}
