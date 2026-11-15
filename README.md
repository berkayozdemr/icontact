# Icontact

Open source eye-contact correction for your webcam, on Windows and macOS.
You look at your screen; the people you talk to see you looking at them.

**Status: early. It tracks your face and corrects your eyes in its own window. It cannot yet send the picture to a meeting app, and sideways glances still look wrong.**

- Runs on any CPU, no graphics card needed.
- Everything stays on your machine. No network calls.
- Written in Rust.

The plan is in [planning/TECHNICAL_SPEC.md](planning/TECHNICAL_SPEC.md).

## Building (milestone 4: eye correction in the preview)

Needs Rust 1.92 or newer. The first build downloads a prebuilt ONNX Runtime
library (the thing that runs the models), so it needs a network connection once.

```bash
cargo build --release
```

```bash
cargo test
```

- `target/release/ec-app` opens a window with the live camera and the eyes
  corrected, with a strength slider and a switch to see the original.
  `--camera N` picks another camera; `--pattern` shows a generated picture and
  `--clip FILE.y4m` a recorded clip instead.
- `target/release/ec-cli` lists cameras, times a camera, benchmarks the pipeline
  stages, runs the face tracker over a clip, and corrects a clip (`ec-cli help`).

Not built yet: the fake camera that Zoom and other apps would pick. So far the
corrected picture is only shown in the app's own window.

## Licence

Code and model files: MIT or Apache-2.0, at your choice. The two face models come from
Google MediaPipe under Apache-2.0; the eye model is our own; see `crates/ec-vision/models/NOTICE.md`.
