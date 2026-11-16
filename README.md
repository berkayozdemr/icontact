# Icontact

Open source eye-contact correction for your webcam, on macOS and Windows.
You look at your screen; the people you talk to see you looking at them.

- Runs on the processor alone; no graphics card needed.
- Everything stays on your machine. The app contains no networking code.
- Free, and built from this repository with one command. No account, no store.
- Written in Rust.

## Status: early

| | macOS | Windows |
|---|---|---|
| Corrects your eyes in its own window | Yes | Builds and passes its tests; not yet tried on a real camera |
| Shows up as a camera in meeting apps | Yes, through OBS's camera (see below) | Windows 11: written, not yet tried on a real machine |

What to expect today:

- **It has been tried by one person on one MacBook.** Other faces, glasses,
  dim rooms and slow machines are untested. Reports are very welcome.
- Seen working in Microsoft Teams. Zoom, FaceTime and browsers are not yet checked.
- It corrects the usual "looking at the screen, a little below the camera"
  case. When you look well away to the side, it leaves your eyes as they are.
- There are no downloads yet: you build it yourself (below).

## On a Mac: one command

Needs macOS 13.4 or newer, Apple's command line tools (`xcode-select --install`)
and Rust (https://rustup.rs). After cloning:

```bash
scripts/setup-mac.sh
```

It builds `Icontact.app`, sets up the camera that meeting apps will pick, and
starts the app. Then pick **"OBS Virtual Camera"** as the camera in your
meeting app.

Why OBS: macOS only lets programs signed with a paid Apple developer account
add a camera of their own, and this project has no such account. So Icontact
borrows the camera that the free program [OBS](https://obsproject.com)
installs. The script offers to install OBS, starts its camera once and closes
it again. One step is yours: macOS asks you to switch the camera on in System
Settings, and the script opens that page and waits. After that OBS never has
to be open.

The app is not signed either. If you copy `Icontact.app` to another Mac, the
first start there is right-click > Open.

## On Windows 11: first draft, untested

Needs Windows 11, Rust (https://rustup.rs) and the Visual Studio C++ build
tools that Rust asks for. After cloning, in PowerShell in the project folder:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1
```

It builds the app and Icontact's own camera, and asks once for administrator
rights to install the camera part. Then start `target\release\ec-app.exe`
and pick **"Icontact Camera"** in your meeting app.

**This Windows route was written on a Mac and has not been run on a real
Windows machine yet.** It compiles and its tests pass, nothing more. If you
try it and the camera does not appear or shows no picture, the file
`C:\ProgramData\Icontact\mfcam.log` says what the camera part did; please
open an issue with it. `scripts\uninstall-windows.ps1` removes everything again.

Reading a real Windows webcam is equally untried. Without a webcam,
`ec-app.exe --pattern` sends a test picture instead.

## Building and testing by hand

Needs Rust 1.92 or newer. The first build downloads a prebuilt ONNX Runtime
library (the thing that runs the models), so it needs a network connection once.

```bash
cargo build --release
```

```bash
cargo test
```

- `target/release/ec-app` opens the window: live camera, eyes corrected, a
  strength slider, a switch to see the original, and (on a Mac) a "Send to
  camera" switch. `--camera N` picks another camera; `--pattern` shows a
  generated picture and `--clip FILE.y4m` a recorded clip instead.
- `target/release/ec-cli` holds the measuring tools: list cameras, time a
  camera, time the pipeline stages, correct a recorded clip, check the OBS
  set-up (`ec-cli help`).
- `scripts/make-mac-app.sh` builds `target/Icontact.app` without touching the
  camera set-up.

## Privacy

- Your camera picture is read, changed and handed to the meeting app on your
  own machine. It is never stored and never sent anywhere by Icontact.
- The app has no networking code at all, so there is no telemetry, no update
  check and no account.

## How it works, briefly

Each camera frame goes through four steps: find the face, find the eyes and
where they look, ask a small model how to shift the pixels of each eye so it
looks at the camera, and lay the changed eyes back into the frame. Only the
eye regions are touched; the rest of the picture is passed through as it came.

The code is commented in plain language throughout.

## Licence

Code and the eye model: MIT or Apache-2.0, at your choice. The two face models
come from Google MediaPipe under Apache-2.0
(`crates/ec-vision/models/NOTICE.md`).
