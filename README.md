# Icontact

Open source eye-contact correction for your webcam, on Windows and macOS.
You look at your screen; the people you talk to see you looking at them.

**Status: early. It tracks your face and corrects your eyes. On a Mac it sends the corrected picture to meeting apps through OBS's camera; on Windows it cannot yet. Sideways glances still look wrong.**

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

On a Mac the app also sends the corrected picture to OBS's camera, so meeting
apps can pick "OBS Virtual Camera". OBS (30 or newer) must be installed and its
virtual camera started once; the app's window lists the steps still to do,
and so does `ec-cli obs-check`. Seen working in Microsoft Teams; OBS does not have to be running.

`scripts/make-mac-app.sh` builds `target/Icontact.app`, an app you can
double-click. It is not signed with an Apple account, so on another Mac the
first start is right-click > Open.
On Windows there is no fake camera yet.

## Licence

Code and model files: MIT or Apache-2.0, at your choice. The two face models come from
Google MediaPipe under Apache-2.0; the eye model is our own; see `crates/ec-vision/models/NOTICE.md`.
