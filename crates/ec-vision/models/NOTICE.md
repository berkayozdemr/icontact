# Model files in this folder

| File | What it is | From | Licence |
|---|---|---|---|
| `face_detector.onnx` | Face detector ("BlazeFace short range") | Google MediaPipe, inside `face_landmarker.task` | Apache-2.0 |
| `face_landmarks_detector.onnx` | Face mesh, 478 points with iris | Google MediaPipe, inside `face_landmarker.task` | Apache-2.0 |
| `warp.onnx` | The eye-turning model, pc2 (round 2: half-size network, more varied training eyes) | Ours: trained with `training/` on eyes rendered from MakeHuman CC0 assets. A copy of `training/results/round2_step3/pc2/warp.onnx` (branch `training/round2`) | MIT OR Apache-2.0 |

The two MediaPipe files were taken out of
`https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/latest/face_landmarker.task`
(a zip file) and converted from TFLite to ONNX with `tf2onnx --tflite`, unchanged
otherwise. On one test crop the converted mesh model gives the same points as
the original to within 0.001 px.

To use a newer warp model, copy its `warp.onnx` over the one here and rebuild.

All three are built into the executable with `include_bytes!`, so the app is one file.
