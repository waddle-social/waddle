# Selfie segmentation model

The Apache-2.0 licensed MediaPipe square selfie segmentation model predicts one foreground confidence value per pixel. Qualcomm's float ONNX export reconstructs the Google model with standard operators. The 256 × 256 input is RGB in the range [0, 1]. Both the graph and the external weights are embedded in the local camera processor.

These files come from the `float.universal_assets.onnx` archive in the [Qualcomm release manifest](https://huggingface.co/qualcomm/MediaPipe-Selfie-Segmentation/blob/3937e58/release_assets.json) for version 0.67.0. See the [model license](https://huggingface.co/qualcomm/MediaPipe-Selfie-Segmentation/blob/3937e58/LICENSE) and [model implementation](https://github.com/qualcomm/ai-hub-models/blob/v0.67.0/src/qai_hub_models/models/mediapipe_selfie/model.py).

| File | SHA-256 |
| --- | --- |
| mediapipe_selfie.onnx | adbdd7afc0903cc9a304e03e50afdda4407b33ba9eff8e1e6522dfbf008e04e3 |
| mediapipe_selfie.data | 91aef851b713db7a7e680ff5a63866e14be07f9dc2c9161812becbe6ba2beb9f |
