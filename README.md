# Icontact

Open source eye-contact correction for your webcam, on Windows and macOS.
You look at your screen; the people you talk to see you looking at them.

**Status: early. It shows the camera and tracks your face. It does not correct anything yet.**

- Runs on any CPU, no graphics card needed.
- Everything stays on your machine. No network calls.
- Written in Rust.

The plan is in [planning/TECHNICAL_SPEC.md](planning/TECHNICAL_SPEC.md).

## Building (milestone 2: camera preview with face tracking)

Needs Rust 1.92 or newer. The first build downloads a prebuilt ONNX Runtime
library (the thing that runs the face models), so it needs a network connection once.

```bash
cargo build --release
```

```bash
cargo test
```

- `target/release/ec-app` opens a window with the live camera and draws the face
  points it finds. `--camera N` picks another camera; `--pattern` shows a generated
  picture and `--clip FILE.y4m` a recorded clip instead.
- `target/release/ec-cli` lists cameras, times a camera, and benchmarks the
  pipeline stages, and runs the face tracker over a clip (`ec-cli help`).

## Licence

Code and model files: MIT or Apache-2.0, at your choice. The two face models come from
Google MediaPipe under Apache-2.0; see `crates/ec-vision/models/NOTICE.md`.
