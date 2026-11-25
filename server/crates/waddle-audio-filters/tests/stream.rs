use waddle_audio_filters::{AudioFilter, Mode};

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(i16::from_le_bytes)
        .collect()
}

fn process(filter: &mut AudioFilter, input: &[i16], chunk: usize) -> Vec<i16> {
    let mut output = input.to_vec();
    for block in output.chunks_mut(chunk) {
        assert!(filter.process(block));
    }
    output
}

fn assert_reference(
    mode: Mode,
    rate: u32,
    hop: usize,
    input: &[u8],
    reference: &[u8],
    max_rms: f64,
    max_error: f64,
) {
    let mut filter = AudioFilter::new(mode, rate, 1).unwrap();
    let input = samples(input);
    // Non-aligned capture callbacks exercise accumulation across model hops.
    let output = process(&mut filter, &input, 97);
    let delay = hop - 1;
    assert!(output[..delay].iter().all(|&x| x == 0));
    let expected = samples(reference);
    let errors: Vec<_> = output[delay..]
        .iter()
        .zip(expected)
        .map(|(&x, y)| f64::from(x) - f64::from(y))
        .collect();
    let rms = (errors.iter().map(|x| x * x).sum::<f64>() / errors.len() as f64).sqrt();
    let peak = errors.iter().fold(0.0f64, |peak, x| peak.max(x.abs()));
    assert!(
        rms <= max_rms && peak <= max_error,
        "reference error RMS {rms}, peak {peak}"
    );
}

#[test]
fn rnnoise_matches_the_browser_wasm_reference() {
    assert_reference(
        Mode::Rnnoise,
        48_000,
        480,
        include_bytes!("data/input-48000.s16"),
        include_bytes!("data/rnnoise-browser.s16"),
        40.0,
        380.0,
    );
}

#[test]
fn dtln_matches_both_original_onnx_and_browser_tflite_models() {
    for reference in [
        include_bytes!("data/dtln-onnxruntime.s16"),
        include_bytes!("data/dtln-browser.s16"),
    ] {
        assert_reference(
            Mode::Dtln,
            16_000,
            128,
            include_bytes!("data/input-16000.s16"),
            reference,
            0.5,
            3.0,
        );
    }
}

#[test]
fn bypass_preserves_every_bit_and_rejects_invalid_geometry() {
    assert!(Mode::from_id(3).is_none());
    for (rate, channels) in [(7_999, 1), (96_001, 2), (48_000, 0), (48_000, 3)] {
        assert!(AudioFilter::new(Mode::Bypass, rate, channels).is_none());
    }
    let mut filter = AudioFilter::new(Mode::Bypass, 8_000, 2).unwrap();
    let mut pcm = [i16::MIN, i16::MAX, 3, -9];
    let original = pcm;
    assert!(filter.process(&mut pcm));
    assert_eq!(pcm, original);
    assert!(!filter.process(&mut pcm[..3]));
    assert_eq!(pcm, original);
    assert!(!filter.process(&mut []));
    assert!(!filter.process(&mut [0; 1602]));
}

#[test]
fn histories_and_resamplers_are_independent_and_reset_discards_pending_audio() {
    let left = samples(include_bytes!("data/input-48000.s16"));
    let right: Vec<_> = left.iter().rev().copied().collect();
    let stereo: Vec<_> = left
        .iter()
        .zip(&right)
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    for mode in [Mode::Rnnoise, Mode::Dtln] {
        let mut pair = AudioFilter::new(mode, 44_100, 2).unwrap();
        let mut mono = AudioFilter::new(mode, 44_100, 1).unwrap();
        let left_out = process(&mut mono, &left, 137);
        mono.reset();
        let right_out = process(&mut mono, &right, 199);
        let expected: Vec<_> = left_out
            .iter()
            .zip(&right_out)
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        let actual = process(&mut pair, &stereo, 246);
        assert_eq!(actual, expected);
        assert!(actual.iter().any(|&v| v != 0));
        assert_ne!(actual, stereo);
        assert!(!pair.process(&mut [5; 3]));
        pair.reset();
        assert_eq!(process(&mut pair, &stereo, 442), expected);
    }
}

#[test]
fn supported_rates_produce_output_with_whole_channel_frames() {
    for rate in [
        8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000,
    ] {
        let mut filter = AudioFilter::new(Mode::Dtln, rate, 1).unwrap();
        let mut input = vec![0; (rate / 100) as usize];
        for _ in 0..4 {
            assert!(filter.process(&mut input));
            assert!(input.iter().all(|&v| v.abs_diff(0) <= 1));
        }
    }
}
