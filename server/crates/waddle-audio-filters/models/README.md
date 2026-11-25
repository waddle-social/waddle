# DTLN model provenance

The native microphone filter uses Nils L. Westhausen’s two-stage DTLN model under the [MIT license](LICENSE). The [upstream pretrained directory](https://github.com/breizhn/DTLN/tree/1de1f15a8b5b7e1c44905618ff2ef70ca8277fbc/pretrained_model) contains both ONNX models. The [upstream streaming reference](https://github.com/breizhn/DTLN/blob/1de1f15a8b5b7e1c44905618ff2ef70ca8277fbc/real_time_processing_onnx.py) defines the tensor shapes, 16 kHz sample rate and overlap-add procedure.

| Upstream file | SHA-256 | Bundled file | Bundled SHA-256 |
| --- | --- | --- | --- |
| model_1.onnx | 22b91cae3855e5a0620e66a917ca6c82c58db0e842c770f58d86751c5e8d4ae3 | dtln_mask.onnx | 0b9f5c0246015c5cf8d004835f6dcc04f292aac52f5a7df4e3aeade2335cd797 |
| model_2.onnx | e20c92f9233fccf29cddf86970d0d0161a03aebccc26d6f4d5639c4d5ec2e639 | dtln_decoder.onnx | e20c92f9233fccf29cddf86970d0d0161a03aebccc26d6f4d5639c4d5ec2e639 |

RTen accepts the seven-input ONNX LSTM operator without peepholes. The upstream spectral mask encodes its unused eighth input as an empty string. To reproduce the bundled protobuf from the upstream file with Python’s `onnx` package, run the following in the directory containing `model_1.onnx`.

```python
import onnx

model = onnx.load("model_1.onnx")
for node in model.graph.node:
    if node.op_type == "LSTM" and len(node.input) == 8:
        assert node.input[7] == ""
        del node.input[7:]
onnx.checker.check_model(model)
onnx.save(model, "dtln_mask.onnx")
```

The waveform regression tests compare the Rust implementation with both the original ONNX pair and the web client’s [packaged TFLite models](https://github.com/workadventure/noise-suppression). The RNNoise filter uses the model bundled in the pinned `nnnoiseless` crate.
