//! Icontact app, milestone 1: a window that shows the live camera (T-024).
//! No correction, tray icon or fake camera yet.
//!
//!     ec-app                 camera 0
//!     ec-app --camera 1      another camera
//!     ec-app --pattern       a generated moving picture instead of a camera
//!     --exit-after SECS      print the read-out and close (for smoke tests)

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ec_capture::{
    request_camera_permission, CameraSource, CaptureError, FrameSource, SyntheticSource,
};
use ec_core::stats::Timings;
use ec_core::{slot, Frame, PixelFormat, SlotReceiver};
use eframe::egui;

enum Source {
    Camera(u32),
    Pattern,
}

/// Status from the capture thread. These go through an ordinary queue, because unlike
/// frames none of them may be dropped: the frame slot would let the first frame
/// overwrite `Opened` before the window had read it.
enum Status {
    Opened(String),
    Failed(String),
}

/// Runs on its own thread: opens the source and pushes every frame into the one-place slot.
fn capture_loop(
    source: Source,
    frames: ec_core::SlotSender<Frame>,
    status: mpsc::Sender<Status>,
    ctx: egui::Context,
) {
    let opened: Result<Box<dyn FrameSource>, CaptureError> = match source {
        Source::Pattern => Ok(Box::new(SyntheticSource::new(
            1280,
            720,
            PixelFormat::Nv12,
            None,
        ))),
        Source::Camera(i) => request_camera_permission(Duration::from_secs(120))
            .and_then(|()| CameraSource::open(i, 1280, 720, 30))
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
    let paced = matches!(source, Source::Pattern);
    let _ = status.send(Status::Opened(src.describe()));
    ctx.request_repaint();
    let mut next = Instant::now();
    loop {
        let failure = match src.next_frame() {
            // an Err from the slot means the window is gone
            Ok(Some(f)) => match frames.send(f) {
                Ok(_) => None,
                Err(_) => return,
            },
            Ok(None) => Some("the source ended".to_string()),
            Err(e) => Some(e.to_string()),
        };
        ctx.request_repaint();
        if let Some(reason) = failure {
            let _ = status.send(Status::Failed(reason));
            return;
        }
        if paced {
            // the test pattern has no clock of its own, so give it 30 frames a second
            next += Duration::from_millis(33);
            thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    }
}

struct App {
    rx: SlotReceiver<Frame>,
    status: mpsc::Receiver<Status>,
    started: Instant,
    exit_after: Option<Duration>,
    texture: Option<egui::TextureHandle>,
    source_line: String,
    error: Option<String>,
    frame_line: String,
    convert: Timings,
    arrivals: Timings,
    last_arrival: Option<Instant>,
    shown: u64,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(s) = self.status.try_recv() {
            match s {
                Status::Opened(line) => self.source_line = line,
                Status::Failed(e) => self.error = Some(e),
            }
        }
        {
            {
                if let Some(f) = self.rx.try_recv() {
                    let now = Instant::now();
                    if let Some(prev) = self.last_arrival.replace(now) {
                        self.arrivals.push((now - prev).as_secs_f64() * 1000.0);
                        self.arrivals.keep_last(90);
                    }
                    let t = Instant::now();
                    let rgb = f.to_rgb();
                    let image =
                        egui::ColorImage::from_rgb([f.width() as usize, f.height() as usize], &rgb);
                    match &mut self.texture {
                        Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                        None => {
                            self.texture = Some(ui.ctx().load_texture(
                                "camera",
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                    }
                    self.convert.push(t.elapsed().as_secs_f64() * 1000.0);
                    self.convert.keep_last(90);
                    self.shown += 1;
                    self.frame_line = format!("{}x{} {:?}", f.width(), f.height(), f.format());
                }
            }
        }
        if self.exit_after.is_some_and(|d| self.started.elapsed() >= d) {
            println!("source: {}", self.source_line);
            println!("frame: {}", self.frame_line);
            println!("shown {} skipped {}", self.shown, self.rx.replaced());
            println!(
                "frame gap median {:?} ms, convert + upload median {:?} ms",
                self.arrivals.median(),
                self.convert.median()
            );
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
                    format!("Stopped: {e}"),
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
            if let Some(tex) = &self.texture {
                let avail = ui.available_size();
                let [w, h] = tex.size();
                let scale = (avail.x / w as f32).min(avail.y / h as f32).max(0.05);
                ui.image((tex.id(), egui::vec2(w as f32 * scale, h as f32 * scale)));
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
        [] => Source::Camera(0),
        ["--pattern"] => Source::Pattern,
        ["--camera", n] if n.parse::<u32>().is_ok() => Source::Camera(n.parse().unwrap_or(0)),
        _ => {
            eprintln!("usage: ec-app [--camera N | --pattern] [--exit-after SECS]");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 640.0])
            .with_title("Icontact"),
        ..Default::default()
    };
    eframe::run_native(
        "Icontact",
        options,
        Box::new(move |cc| {
            let (tx, rx) = slot();
            let (status_tx, status) = mpsc::channel();
            let ctx = cc.egui_ctx.clone();
            thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_loop(source, tx, status_tx, ctx))
                .expect("could not start the capture thread");
            Ok(Box::new(App {
                rx,
                status,
                started: Instant::now(),
                exit_after,
                texture: None,
                source_line: String::new(),
                error: None,
                frame_line: String::new(),
                convert: Timings::default(),
                arrivals: Timings::default(),
                last_arrival: None,
                shown: 0,
            }))
        }),
    )
}
