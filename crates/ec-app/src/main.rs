//! Icontact app, milestone 4: a window that shows the live camera (T-024) with the
//! eyes corrected, the face tracker's points drawn on top (T-036), and a read-out
//! of the gaze and the gate.
//! No tray icon, calibration screen or fake camera yet.
//!
//!     ec-app                 camera 0
//!     ec-app --camera 1      another camera
//!     ec-app --pattern       a generated moving picture instead of a camera
//!     ec-app --clip F.y4m    a recorded clip, looped, instead of a camera
//!     --exit-after SECS      print the read-out and close (for smoke tests)

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use ec_capture::{
    request_camera_permission, CameraSource, CaptureError, FrameSource, SyntheticSource, Y4mSource,
};
use ec_core::stats::Timings;
use ec_core::{slot, Frame, PixelFormat, SlotReceiver, SlotSender};
use ec_vision::{points, Correction, Face, Level, Pipeline};
use eframe::egui;

enum Source {
    /// A camera by its number, or `None` for the first real one.
    Camera(Option<u32>),
    Pattern,
    Clip(String),
}

/// Status from the worker threads. These go through an ordinary queue, because unlike
/// frames none of them may be dropped: the frame slot would let the first frame
/// overwrite `Opened` before the window had read it.
enum Status {
    Opened(String),
    Failed(String),
    /// What is happening with the fake camera, as one line for the window.
    /// The camera-output line, and the set-up steps still to do (empty when none).
    Output(String, &'static [&'static str]),
}

/// Runs on its own thread: opens the source and pushes every frame into the one-place slot.
fn capture_loop(
    source: Source,
    frames: SlotSender<Frame>,
    status: mpsc::Sender<Status>,
    ctx: egui::Context,
) {
    let opened: Result<Box<dyn FrameSource>, CaptureError> = match &source {
        Source::Pattern => Ok(Box::new(SyntheticSource::new(
            1280,
            720,
            PixelFormat::Nv12,
            None,
        ))),
        Source::Clip(path) => {
            Y4mSource::open(path, true).map(|c| Box::new(c) as Box<dyn FrameSource>)
        }
        // No number given: the first real camera, never a fake one such as
        // OBS's (that is where our own picture goes out).
        Source::Camera(i) => request_camera_permission(Duration::from_secs(120))
            .and_then(|()| match i {
                Some(i) => CameraSource::open(*i, 1280, 720, 30),
                None => CameraSource::open_default(1280, 720, 30),
            })
            .map(|c| Box::new(c) as Box<dyn FrameSource>),
    };
    let mut src = match opened {
        Ok(s) => s,
        Err(e) => {
            let _ = status.send(Status::Failed(e.to_string()));
            ctx.request_repaint();
            return;
        }
    };
    let paced = !matches!(source, Source::Camera(_));
    let _ = status.send(Status::Opened(src.describe()));
    ctx.request_repaint();
    let mut next = Instant::now();
    loop {
        let failure = match src.next_frame() {
            // an Err from the slot means the pipeline thread is gone
            Ok(Some(f)) => match frames.send(f) {
                Ok(_) => None,
                Err(_) => return,
            },
            Ok(None) => Some("the source ended".to_string()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(reason) = failure {
            let _ = status.send(Status::Failed(reason));
            ctx.request_repaint();
            return;
        }
        if paced {
            // a test pattern or clip has no clock of its own, so give it 30 frames a second
            next += Duration::from_millis(33);
            thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    }
}

/// The switches the window sets and the pipeline thread reads for every frame.
/// Each is one value that two threads may touch without a lock ("atomic").
struct Controls {
    tracking: AtomicBool,
    correcting: AtomicBool,
    /// The strength slider, 0 to 1. There is no atomic `f32`, so the number's
    /// 32 bits are kept in an atomic `u32` (`to_bits` / `from_bits`).
    strength: AtomicU32,
    /// Send the finished frames to the fake camera that meeting apps pick.
    sending: AtomicBool,
}

/// The way out to meeting apps. On a Mac that is OBS's installed camera. On
/// Windows 11 it is our own "Icontact Camera": the frames go into a shared
/// block (`ec_sink::Writer`) and the camera library reads them there.
struct Output {
    /// Windows: the shared block the finished frames are written into.
    #[cfg(windows)]
    writer: Option<ec_sink::Writer>,
    /// Windows: the camera, switched on. Dropping it takes it off Windows' list.
    #[cfg(windows)]
    win_camera: Option<ec_sink::mf_win::WinCamera>,
    /// Windows: when setting up was last tried, to retry every two seconds only.
    #[cfg(windows)]
    win_tried: Option<Instant>,
    #[cfg(target_os = "macos")]
    camera: Option<ec_sink::obs_mac::ObsCamera>,
    /// The size the camera was opened for; a frame of another size reopens it.
    #[cfg(target_os = "macos")]
    size: (u32, u32),
    /// When opening the camera was last tried, so a missing camera is not asked for 30 times a second.
    #[cfg(target_os = "macos")]
    tried: Option<Instant>,
    /// The last line reported, to report only changes.
    line: String,
    /// The set-up steps the window should show under that line (T-072).
    steps: &'static [&'static str],
}

/// What `Output::send` reports when something changed: the line and the steps.
type OutputNews = (String, &'static [&'static str]);

impl Output {
    fn new() -> Output {
        Output {
            #[cfg(windows)]
            writer: None,
            #[cfg(windows)]
            win_camera: None,
            #[cfg(windows)]
            win_tried: None,
            #[cfg(target_os = "macos")]
            camera: None,
            #[cfg(target_os = "macos")]
            size: (0, 0),
            #[cfg(target_os = "macos")]
            tried: None,
            line: String::new(),
            steps: &[],
        }
    }

    /// Report the line and steps, but only when they differ from last time.
    fn news(&mut self, line: String, steps: &'static [&'static str]) -> Option<OutputNews> {
        (line != self.line || steps != self.steps).then(|| {
            self.line = line.clone();
            self.steps = steps;
            (line, steps)
        })
    }

    /// Hand one finished frame to the fake camera, if sending is on. Never
    /// stops the pipeline: when the camera is missing or refuses, the reason
    /// goes to the window and the next attempt waits two seconds.
    #[cfg(target_os = "macos")]
    fn send(&mut self, frame: &Frame, on: bool) -> Option<OutputNews> {
        use ec_sink::obs_mac::{obs_state, ObsCamera};
        let size = (frame.width(), frame.height());
        let mut steps = self.steps;
        let line = if !on {
            steps = &[];
            self.camera = None; // closing it stops the camera
            self.tried = None;
            "off".to_string()
        } else {
            if self.camera.is_some() && self.size != size {
                self.camera = None;
                self.tried = None;
            }
            let may_try = self
                .tried
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            let mut problem = None;
            if self.camera.is_none() && may_try {
                self.tried = Some(Instant::now());
                match ObsCamera::open(size.0, size.1) {
                    Ok(c) => (self.camera, self.size) = (Some(c), size),
                    Err(e) => {
                        // Say why in the user's terms: if the one-time OBS
                        // set-up is not finished, show its steps instead of
                        // the raw error. Asked only here, every two seconds.
                        let state = obs_state();
                        steps = state.steps();
                        problem = Some(if steps.is_empty() {
                            e.to_string()
                        } else {
                            state.headline().to_string()
                        });
                    }
                }
            }
            if let Some(c) = &mut self.camera {
                steps = &[];
                if let Err(e) = c.send(frame) {
                    problem = Some(e.to_string());
                    self.camera = None;
                }
            }
            match (problem, &self.camera) {
                (Some(p), _) => format!("not sending: {p}"),
                (None, Some(_)) => format!(
                    "sending {}x{} to \"OBS Virtual Camera\" (pick that camera in your meeting app)",
                    size.0, size.1
                ),
                // between two attempts: keep showing why the last one failed
                (None, None) => self.line.clone(),
            }
        };
        self.news(line, steps)
    }

    /// Windows: put the frame into the shared block and keep the camera
    /// switched on. Like the Mac version it never stops the pipeline; a
    /// problem goes to the window and the next attempt waits two seconds.
    ///
    /// Written on a Mac; reported working on one Windows 11 PC (2026-11-16).
    #[cfg(windows)]
    fn send(&mut self, frame: &Frame, on: bool) -> Option<OutputNews> {
        use ec_sink::mf_win::{self, WinCamera};
        use ec_sink::{layout, Writer, WIN_CAMERA_NAME};
        /// Shown in the window while the one-time install is missing.
        const INSTALL: &[&str] = &[
            "Close Icontact.",
            "In PowerShell, in the project folder, run: powershell -ExecutionPolicy Bypass -File scripts\\install-windows.ps1",
            "Say yes when Windows asks for administrator rights, then start Icontact again.",
        ];
        let mut steps: &'static [&'static str] = &[];
        let line = if !on {
            // dropping these removes the camera and marks the block "closed"
            (self.win_camera, self.writer, self.win_tried) = (None, None, None);
            "off".to_string()
        } else if !mf_win::installed() {
            (self.win_camera, self.writer) = (None, None);
            steps = INSTALL;
            "not sending: the camera part is not installed yet".to_string()
        } else {
            let may_try = self
                .win_tried
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            let mut problem = None;
            // 1. the shared block
            if self.writer.is_none() && may_try {
                self.win_tried = Some(Instant::now());
                match Writer::create(ec_sink::default_path(), layout::DEFAULT_ROOM) {
                    Ok(w) => self.writer = Some(w),
                    Err(e) => problem = Some(format!("the shared block: {e}")),
                }
            }
            // 2. this frame into it
            if let Some(w) = &mut self.writer {
                if let Err(e) = w.write(frame) {
                    problem = Some(format!("writing a frame: {e}"));
                    self.writer = None;
                }
            }
            // 3. the camera, once the block exists
            if self.writer.is_some() && self.win_camera.is_none() && may_try {
                self.win_tried = Some(Instant::now());
                match WinCamera::start() {
                    Ok(c) => self.win_camera = Some(c),
                    Err(e) => problem = Some(e.to_string()),
                }
            }
            match (problem, self.writer.is_some() && self.win_camera.is_some()) {
                (Some(p), _) => format!("not sending: {p}"),
                (None, true) => format!(
                    "sending {}x{} to \"{WIN_CAMERA_NAME}\" (pick that camera in your meeting app)",
                    frame.width(),
                    frame.height()
                ),
                // between two attempts: keep showing why the last one failed
                (None, false) => self.line.clone(),
            }
        };
        self.news(line, steps)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn send(&mut self, _frame: &Frame, _on: bool) -> Option<OutputNews> {
        self.news("not built for this system yet".to_string(), &[])
    }
}

/// What the pipeline thread hands the window: the frame and what was found in it.
struct Tracked {
    /// The frame as the camera sent it. Shares its pixels with `frame` unless the eyes were changed.
    original: Frame,
    /// The frame after the pipeline.
    frame: Frame,
    face: Option<Face>,
    correction: Option<Correction>,
    level: Level,
    /// Time spent on this frame by the whole pipeline, in milliseconds. `None`: tracking was off.
    pipeline_ms: Option<f64>,
}

/// Runs on its own thread: takes each frame from the capture thread, runs the
/// pipeline (find the face, correct the eyes), and passes the result on to the window.
fn pipeline_loop(
    frames: SlotReceiver<Frame>,
    out: SlotSender<Tracked>,
    status: mpsc::Sender<Status>,
    controls: Arc<Controls>,
    ctx: egui::Context,
) {
    let mut pipeline = match Pipeline::new(1) {
        Ok(p) => Some(p),
        Err(e) => {
            let _ = status.send(Status::Failed(format!("eye correction is off: {e}")));
            None
        }
    };
    let mut output = Output::new();
    let mut first_frame_at = None;
    // `recv` waits for the next frame, and returns `None` once the capture thread is gone
    while let Some(frame) = frames.recv() {
        let epoch = *first_frame_at.get_or_insert(frame.captured_at);
        let time = frame
            .captured_at
            .saturating_duration_since(epoch)
            .as_secs_f64();
        let mut tracked = Tracked {
            original: frame.clone(), // shares the pixels; no copy
            frame: frame.clone(),
            face: None,
            correction: None,
            level: Level::Full,
            pipeline_ms: None,
        };
        if !controls.tracking.load(Ordering::Relaxed) {
            if let Some(p) = pipeline.as_mut() {
                p.reset();
            }
        } else if let Some(p) = pipeline.as_mut() {
            p.correcting = controls.correcting.load(Ordering::Relaxed);
            p.corrector.settings.strength =
                f32::from_bits(controls.strength.load(Ordering::Relaxed));
            match p.process(frame, time) {
                Ok(done) => {
                    tracked.frame = done.frame;
                    tracked.face = done.face;
                    tracked.correction = done.correction;
                    tracked.level = done.level;
                    tracked.pipeline_ms = (!done.repeated).then_some(done.total_ms);
                }
                Err(e) => {
                    let _ = status.send(Status::Failed(format!("eye correction stopped: {e}")));
                    pipeline = None;
                }
            }
        }
        // Stage 6b: out to the meeting apps. With correction off this is the
        // camera's own picture, passed through.
        let sending = controls.sending.load(Ordering::Relaxed);
        if let Some((line, steps)) = output.send(&tracked.frame, sending) {
            let _ = status.send(Status::Output(line, steps));
        }
        // an Err from the slot means the window is gone
        if out.send(tracked).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

struct App {
    rx: SlotReceiver<Tracked>,
    status: mpsc::Receiver<Status>,
    /// Shared with the pipeline thread, which reads it for every frame.
    controls: Arc<Controls>,
    show_points: bool,
    /// Show the camera's own picture instead of the corrected one, to compare.
    show_original: bool,
    face: Option<Face>,
    correction: Option<Correction>,
    level: Level,
    corrected: u64,
    started: Instant,
    exit_after: Option<Duration>,
    texture: Option<egui::TextureHandle>,
    source_line: String,
    output_line: String,
    /// The OBS set-up steps still to do, shown as a numbered list; empty when none.
    output_steps: &'static [&'static str],
    error: Option<String>,
    frame_line: String,
    convert: Timings,
    arrivals: Timings,
    track: Timings,
    last_arrival: Option<Instant>,
    shown: u64,
    with_face: u64,
}

impl App {
    /// Take the newest frame, if one is waiting, and load it as the picture to draw.
    fn take_frame(&mut self, ctx: &egui::Context) {
        let Some(tracked) = self.rx.try_recv() else {
            return;
        };
        let f = if self.show_original {
            tracked.original
        } else {
            tracked.frame
        };
        let now = Instant::now();
        if let Some(prev) = self.last_arrival.replace(now) {
            self.arrivals.push((now - prev).as_secs_f64() * 1000.0);
            self.arrivals.keep_last(90);
        }
        let t = Instant::now();
        let rgb = f.to_rgb();
        let image = egui::ColorImage::from_rgb([f.width() as usize, f.height() as usize], &rgb);
        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture("camera", image, egui::TextureOptions::LINEAR))
            }
        }
        self.convert.push(t.elapsed().as_secs_f64() * 1000.0);
        self.convert.keep_last(90);
        if let Some(ms) = tracked.pipeline_ms {
            self.track.push(ms);
            self.track.keep_last(90);
        }
        self.shown += 1;
        self.with_face += tracked.face.is_some() as u64;
        self.corrected += tracked.correction.is_some_and(|c| c.changed_pixels > 0) as u64;
        self.face = tracked.face;
        self.correction = tracked.correction;
        self.level = tracked.level;
        self.frame_line = format!("{}x{} {:?}", f.width(), f.height(), f.format());
    }

    fn tracking_line(&self) -> String {
        match (&self.face, self.track.median()) {
            (Some(f), Some(ms)) => format!(
                "pipeline {ms:.1} ms   sure {:.0}%   head: turn {:+.0}°  nod {:+.0}°  tilt {:+.0}°   eyes open {:.2} / {:.2}",
                f.confidence * 100.0,
                f.head.yaw,
                f.head.pitch,
                f.head.roll,
                f.eye_open[0],
                f.eye_open[1],
            ),
            (None, Some(ms)) => format!("pipeline {ms:.1} ms   no face"),
            _ => String::new(),
        }
    }

    /// The gaze and gate read-out (the rest of T-036): where the eyes look, how
    /// strongly each is corrected, and which reason is holding the correction back.
    fn correction_line(&self) -> String {
        let Some(c) = &self.correction else {
            return String::new();
        };
        let gaze = match c.gaze {
            Some(g) => format!("gaze: up {:+.0}°  right {:+.0}°", g[0], g[1]),
            None => "gaze: unreadable".into(),
        };
        // the gate's factors of the weaker eye, smallest first
        let weaker = if c.strength[0] <= c.strength[1] { 0 } else { 1 };
        let f = c.factors[weaker];
        let mut reasons = [
            ("far gaze", f.turn),
            ("sideways glance", f.side),
            ("head turned", f.head),
            ("eye closing", f.open),
            ("unsure", f.sure),
            ("slider", f.user),
        ];
        reasons.sort_by(|a, b| a.1.total_cmp(&b.1));
        let held = if reasons[0].1 < 0.995 {
            format!(
                "   held back by: {} ({:.0}%)",
                reasons[0].0,
                reasons[0].1 * 100.0
            )
        } else {
            String::new()
        };
        format!(
            "{gaze}   strength {:.0}% / {:.0}%   turn: up {:+.0}°  right {:+.0}°{held}   work level: {}",
            c.strength[0] * 100.0,
            c.strength[1] * 100.0,
            c.turn[weaker ^ 1][0],
            c.turn[weaker ^ 1][1],
            self.level.name(),
        )
    }
}

/// Draw what the tracker found on top of the picture (T-036).
/// `origin` is the picture's top-left corner on screen, `scale` is screen points per frame pixel.
fn draw_points(painter: &egui::Painter, origin: egui::Pos2, scale: f32, face: &Face) {
    let at = |x: f32, y: f32| origin + egui::vec2(x, y) * scale;
    let faint = egui::Color32::from_rgba_unmultiplied(120, 220, 255, 110);
    let eye = egui::Color32::from_rgb(90, 255, 120);
    let iris = egui::Color32::from_rgb(255, 210, 60);

    // the square the mesh model looked at
    let corners: Vec<egui::Pos2> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .map(|&(u, v)| {
            let (x, y) = face.roi.to_frame(u, v);
            at(x, y)
        })
        .collect();
    painter.add(egui::Shape::closed_line(
        corners,
        egui::Stroke::new(1.0, faint),
    ));

    for p in &face.points {
        painter.circle_filled(at(p[0], p[1]), 1.0, faint);
    }
    let mark = |i: usize, radius: f32, colour: egui::Color32| {
        if let Some(p) = face.points.get(i) {
            painter.circle_filled(at(p[0], p[1]), radius, colour);
        }
    };
    for i in [
        points::LEFT_EYE_OUTER,
        points::LEFT_EYE_INNER,
        points::LEFT_EYE_TOP,
        points::LEFT_EYE_BOTTOM,
        points::RIGHT_EYE_INNER,
        points::RIGHT_EYE_OUTER,
        points::RIGHT_EYE_TOP,
        points::RIGHT_EYE_BOTTOM,
    ] {
        mark(i, 2.5, eye);
    }
    for i in points::LEFT_IRIS.chain(points::RIGHT_IRIS) {
        mark(i, 2.0, iris);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(s) = self.status.try_recv() {
            match s {
                Status::Opened(line) => self.source_line = line,
                Status::Failed(e) => self.error = Some(e),
                Status::Output(line, steps) => {
                    self.output_line = line;
                    self.output_steps = steps;
                }
            }
        }
        self.take_frame(ui.ctx());
        if self.exit_after.is_some_and(|d| self.started.elapsed() >= d) {
            println!("source: {}", self.source_line);
            println!("frame: {}", self.frame_line);
            println!("shown {} skipped {}", self.shown, self.rx.replaced());
            println!(
                "frame gap median {:?} ms, convert + upload median {:?} ms",
                self.arrivals.median(),
                self.convert.median()
            );
            println!(
                "face in {} of {} frames shown, eyes corrected in {}, pipeline median {:?} ms",
                self.with_face,
                self.shown,
                self.corrected,
                self.track.median()
            );
            println!("camera output: {}", self.output_line);
            for (n, step) in self.output_steps.iter().enumerate() {
                println!("  {}. {step}", n + 1);
            }
            println!("error: {:?}", self.error);
            self.exit_after = None; // print once; the window takes a frame or two to close
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(format!(
                "Source: {}",
                if self.source_line.is_empty() {
                    "opening…"
                } else {
                    &self.source_line
                }
            ));
            if let Some(e) = &self.error {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 60, 60),
                    format!("Problem: {e}"),
                );
            }
            let fps = self.arrivals.median().map(|ms| 1000.0 / ms);
            ui.label(format!(
                "Frame: {}   {} fps   convert + upload {} ms   shown {}   skipped {}",
                if self.frame_line.is_empty() {
                    "-"
                } else {
                    &self.frame_line
                },
                fps.map_or("-".into(), |f| format!("{f:.1}")),
                self.convert
                    .median()
                    .map_or("-".into(), |m| format!("{m:.2}")),
                self.shown,
                self.rx.replaced(),
            ));
            ui.horizontal(|ui| {
                let mut on = self.controls.tracking.load(Ordering::Relaxed);
                if ui.checkbox(&mut on, "Face tracking").changed() {
                    self.controls.tracking.store(on, Ordering::Relaxed);
                }
                ui.checkbox(&mut self.show_points, "Show points");
                ui.label(self.tracking_line());
            });
            ui.horizontal(|ui| {
                let mut on = self.controls.correcting.load(Ordering::Relaxed);
                if ui.checkbox(&mut on, "Correct eyes").changed() {
                    self.controls.correcting.store(on, Ordering::Relaxed);
                }
                let mut strength = f32::from_bits(self.controls.strength.load(Ordering::Relaxed));
                let slider = egui::Slider::new(&mut strength, 0.0..=1.0).text("strength");
                if ui.add(slider).changed() {
                    self.controls
                        .strength
                        .store(strength.to_bits(), Ordering::Relaxed);
                }
                ui.checkbox(&mut self.show_original, "Show original");
            });
            ui.label(self.correction_line());
            ui.horizontal(|ui| {
                let mut on = self.controls.sending.load(Ordering::Relaxed);
                if ui.checkbox(&mut on, "Send to camera").changed() {
                    self.controls.sending.store(on, Ordering::Relaxed);
                }
                ui.label(format!("Camera output: {}", self.output_line));
            });
            // The one-time set-up guide (T-072). It goes away by itself: the
            // pipeline tries the camera again every two seconds.
            if !self.output_steps.is_empty() {
                ui.group(|ui| {
                    ui.label(
                        "To send your picture to meeting apps, one set-up step is needed once:",
                    );
                    for (n, step) in self.output_steps.iter().enumerate() {
                        ui.label(format!("{}. {step}", n + 1));
                    }
                    ui.label("This box goes away by itself when the camera is found.");
                });
            }
            if let Some(tex) = &self.texture {
                let avail = ui.available_size();
                let [w, h] = tex.size();
                let scale = (avail.x / w as f32).min(avail.y / h as f32).max(0.05);
                let shown = ui.image((tex.id(), egui::vec2(w as f32 * scale, h as f32 * scale)));
                if let (true, Some(face)) = (self.show_points, &self.face) {
                    draw_points(ui.painter(), shown.rect.min, scale, face);
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut exit_after = None;
    if let Some(i) = args.iter().position(|a| a == "--exit-after") {
        exit_after = args
            .get(i + 1)
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        if exit_after.is_none() {
            eprintln!("--exit-after needs a number of seconds");
            std::process::exit(2);
        }
        args.drain(i..i + 2);
    }
    let source = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Source::Camera(None),
        ["--pattern"] => Source::Pattern,
        ["--clip", path] => Source::Clip(path.to_string()),
        ["--camera", n] if n.parse::<u32>().is_ok() => Source::Camera(n.parse().ok()),
        _ => {
            eprintln!(
                "usage: ec-app [--camera N | --pattern | --clip FILE.y4m] [--exit-after SECS]"
            );
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 660.0])
            .with_title("Icontact"),
        ..Default::default()
    };
    eframe::run_native(
        "Icontact",
        options,
        Box::new(move |cc| {
            // capture thread -> one-frame slot -> pipeline thread -> one-frame slot -> this window
            let (frame_tx, frame_rx) = slot();
            let (tracked_tx, rx) = slot();
            let (status_tx, status) = mpsc::channel();
            let controls = Arc::new(Controls {
                tracking: AtomicBool::new(true),
                correcting: AtomicBool::new(true),
                strength: AtomicU32::new(ec_vision::DEFAULT_STRENGTH.to_bits()),
                sending: AtomicBool::new(true),
            });

            let (ctx, status_for_capture) = (cc.egui_ctx.clone(), status_tx.clone());
            thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_loop(source, frame_tx, status_for_capture, ctx))
                .expect("could not start the capture thread");

            let (ctx, flag) = (cc.egui_ctx.clone(), controls.clone());
            thread::Builder::new()
                .name("pipeline".into())
                .spawn(move || pipeline_loop(frame_rx, tracked_tx, status_tx, flag, ctx))
                .expect("could not start the pipeline thread");

            Ok(Box::new(App {
                rx,
                status,
                controls,
                show_points: false,
                show_original: false,
                face: None,
                correction: None,
                level: Level::Full,
                corrected: 0,
                started: Instant::now(),
                exit_after,
                texture: None,
                source_line: String::new(),
                output_line: String::new(),
                output_steps: &[],
                error: None,
                frame_line: String::new(),
                convert: Timings::default(),
                arrivals: Timings::default(),
                track: Timings::default(),
                last_arrival: None,
                shown: 0,
                with_face: 0,
            }))
        }),
    )
}
