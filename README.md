# Icontact

Open source eye-contact correction for your webcam, on Windows and macOS.
You look at your screen; the people you talk to see you looking at them.

**Status: planning and early experiments. Nothing usable yet.**

- Runs on any CPU, no graphics card needed.
- Everything stays on your machine. No network calls.
- Written in Rust.

The plan is in [planning/TECHNICAL_SPEC.md](planning/TECHNICAL_SPEC.md).

## Building (milestone 1: camera preview only)

Needs Rust 1.92 or newer.

```bash
cargo build --release
```

```bash
cargo test
```

- `target/release/ec-app` opens a window with the live camera. `--pattern` shows a
  generated picture instead; `--camera N` picks another camera.
- `target/release/ec-cli` lists cameras, times a camera, and benchmarks the
  pipeline stages (`ec-cli help`).

## Licence

Code and model files: MIT or Apache-2.0, at your choice.
