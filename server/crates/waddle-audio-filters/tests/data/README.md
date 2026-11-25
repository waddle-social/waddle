# Offline waveform references

The fixtures contain little-endian signed mono PCM16. Both inputs are synthetic voiced harmonics mixed with pseudorandom noise. Each holds 48 model frames, or 0.48 s at 48 kHz for RNNoise and 0.384 s at 16 kHz for DTLN. The fixtures contain no microphone recordings.

- `rnnoise-browser.s16` is the output of `@sapphi-red/web-noise-suppressor@0.3.5`’s scalar `dist/rnnoise.wasm` (SHA-256 `8b60a2ab88fdae2d1a9f940249d0eb072f28ba8e796f7304347b4e07839c8853`). A fresh RNNoise state processes 480 samples per call, using the worklet’s normalized float to 32767 scaling.
- `dtln-onnxruntime.s16` is the output of ONNX Runtime’s CPU provider with the original, unmodified [DTLN weights](../../models/README.md).
- `dtln-browser.s16` is the output of Google LiteRT 2.4.0 using the two files under `dist/assets` in `@workadventure/noise-suppression@0.0.4`. The SHA-256 sums are `91281a38e80fe9fd330e28eda7e16fe4e483ee5199a3e687a099939013c25de0` for `model_quant_1.tflite` and `7ae37ec802862d8a65b5cdabfbcbbe22caaf7cd39e79adf574d15837d1520830` for `model_quant_2.tflite`. The two DTLN references use a 512-sample rectangular window, 128-sample hop, zero-initialized recurrent states, and NumPy's real FFT and inverse FFT for analysis and overlap-add synthesis.

Each reference stores model output in hop order. The streaming API buffers one model hop before its first output sample. The tests align by this fixed buffering delay and compare waveforms in PCM16 units.
