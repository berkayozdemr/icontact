//! Icontact command-line tool: list cameras, time a camera, and benchmark the
//! pipeline stages on a clip or a generated picture (T-025).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ec_capture::{
    list_cameras, request_camera_permission, CameraSource, FrameSource, SyntheticSource, Y4mSource,
    Y4mWriter,
};
use ec_core::stats::Timings;
use ec_core::{PixelFormat, Rect};

const USAGE: &str = "\
ec-cli cameras
    List the cameras the system offers.
ec-cli capture [--camera N] [--frames N] [--size WxH] [--save FILE.ppm]
    Open a camera, report what it delivers and how evenly frames arrive.
    --save writes the last frame as a picture, to check colours by eye.
ec-cli bench (--clip FILE.y4m | --synthetic nv12|i420|yuyv|rgb|bgr) [--frames N] [--size WxH]
    Time each pipeline stage. --size applies to --synthetic only.
ec-cli make-clip FILE.y4m [--frames N] [--size WxH]
    Write a generated test clip.";

struct Args(Vec<String>);

impl Args {
    /// The value after `--name`, removed from the list.
    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        match self.0.iter().position(|a| a == name) {
            None => Ok(None),
            Some(i) if i + 1 < self.0.len() => {
                self.0.remove(i);
                Ok(Some(self.0.remove(i)))
            }
            Some(_) => Err(format!("{name} needs a value")),
        }
    }

    fn number<T: std::str::FromStr>(&mut self, name: &str, default: T) -> Result<T, String> {
        match self.value(name)? {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|_| format!("{name}: \"{v}\" is not a number")),
        }
    }

    fn size(&mut self) -> Result<(u32, u32), String> {
        match self.value("--size")? {
            None => Ok((1280, 720)),
            Some(v) => v
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .filter(|(w, h): &(u32, u32)| {
                    *w > 0 && *h > 0 && w % 2 == 0 && h % 2 == 0 && *w <= 8192 && *h <= 8192
                })
                .ok_or(format!(
                    "--size: \"{v}\" must look like 1280x720, even numbers"
                )),
        }
    }

    fn done(self) -> Result<(), String> {
        match self.0.first() {
            None => Ok(()),
            Some(a) => Err(format!("unexpected argument \"{a}\"")),
        }
    }
}

fn format_from(name: &str) -> Result<PixelFormat, String> {
    Ok(match name {
        "nv12" => PixelFormat::Nv12,
        "i420" => PixelFormat::I420,
        "yuyv" => PixelFormat::Yuyv,
        "rgb" => PixelFormat::Rgb24,
        "bgr" => PixelFormat::Bgr24,
        _ => return Err(format!("--synthetic: unknown format \"{name}\"")),
    })
}

fn line(name: &str, t: &Timings) {
    match (t.median(), t.p95()) {
        (Some(m), Some(p)) => println!("  {name:<34} median {m:>7.3} ms   p95 {p:>7.3} ms"),
        _ => println!("  {name:<34} no samples"),
    }
}

fn cameras() -> Result<(), String> {
    request_camera_permission(Duration::from_secs(60)).map_err(|e| e.to_string())?;
    let found = list_cameras().map_err(|e| e.to_string())?;
    if found.is_empty() {
        println!("no cameras found");
    }
    for c in found {
        println!("{}: {}", c.index, c.name);
    }
    Ok(())
}

fn capture(mut a: Args) -> Result<(), String> {
    let index: u32 = a.number("--camera", 0)?;
    let frames: usize = a.number("--frames", 90)?;
    let save = a.value("--save")?;
    let (w, h) = a.size()?;
    a.done()?;
    if frames < 2 {
        return Err("--frames must be at least 2".into());
    }
    request_camera_permission(Duration::from_secs(60)).map_err(|e| e.to_string())?;
    let mut cam = CameraSource::open(index, w, h, 30).map_err(|e| e.to_string())?;
    println!("{}", cam.describe());
    let offered = cam.offered_formats();
    println!("modes offered ({}):", offered.len());
    for m in &offered {
        println!("  {m}");
    }
    let mut gaps = Timings::default();
    let mut last = Instant::now();
    let mut last_frame = None;
    for i in 0..frames {
        let f = cam
            .next_frame()
            .map_err(|e| e.to_string())?
            .ok_or("camera ended")?;
        if i == 0 {
            println!(
                "first frame: {}x{} {:?}, {} bytes",
                f.width(),
                f.height(),
                f.format(),
                f.data().len()
            );
        } else if i >= frames / 3 {
            // the first third is skipped: cameras take a moment to settle their exposure and pace
            gaps.push(last.elapsed().as_secs_f64() * 1000.0);
        }
        last = Instant::now();
        last_frame = Some(f);
    }
    if let Some(f) = &last_frame {
        if f.format() == PixelFormat::Yuyv {
            // In YUYV the even bytes are brightness, which varies a lot across a picture, and
            // the odd bytes are colour, which varies little. If the camera really sends UYVY
            // under a YUYV label, the two are the other way round.
            let spread = |offset: usize| {
                let v: Vec<f64> = f
                    .data()
                    .iter()
                    .skip(offset)
                    .step_by(2)
                    .map(|b| *b as f64)
                    .collect();
                let mean = v.iter().sum::<f64>() / v.len() as f64;
                (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
            };
            let (even, odd) = (spread(0), spread(1));
            println!("byte spread: even positions {even:.1}, odd positions {odd:.1}");
            println!(
                "  -> {}",
                if even > odd {
                    "brightness is in the even bytes: really YUYV"
                } else {
                    "brightness is in the odd bytes: this is UYVY mislabelled as YUYV"
                }
            );
        }
        if let Some(path) = &save {
            let mut out = format!("P6\n{} {}\n255\n", f.width(), f.height()).into_bytes();
            out.extend_from_slice(&f.to_rgb());
            std::fs::write(path, out).map_err(|e| e.to_string())?;
            println!("saved the last frame to {path}");
        }
    }
    println!("time between frames over the last {} frames:", gaps.len());
    line("frame gap", &gaps);
    Ok(())
}

fn bench(mut a: Args) -> Result<(), String> {
    let clip = a.value("--clip")?;
    let synthetic = a.value("--synthetic")?;
    let frames: usize = a.number("--frames", 300)?;
    let (w, h) = a.size()?;
    a.done()?;
    let mut source: Box<dyn FrameSource> = match (clip, synthetic) {
        (Some(path), None) => Box::new(Y4mSource::open(&path, true).map_err(|e| e.to_string())?),
        (None, Some(name)) => Box::new(SyntheticSource::new(w, h, format_from(&name)?, None)),
        _ => return Err("bench needs exactly one of --clip or --synthetic".into()),
    };
    println!("source: {}", source.describe());
    let (mut read, mut whole, mut crops, mut total) = (
        Timings::default(),
        Timings::default(),
        Timings::default(),
        Timings::default(),
    );
    let mut checksum = 0u64;
    for i in 0..frames {
        let t0 = Instant::now();
        let Some(frame) = source.next_frame().map_err(|e| e.to_string())? else {
            break;
        };
        let t1 = Instant::now();
        let rgb = frame.to_rgb();
        let t2 = Instant::now();
        // two eye-sized regions, where a face would have them: what the pipeline will convert per frame (D-6)
        let (ew, eh) = ((frame.width() / 10).max(2), (frame.height() / 10).max(2));
        let left = frame
            .crop_rgb(Rect {
                x: frame.width() * 4 / 10 - ew / 2,
                y: frame.height() * 4 / 10,
                w: ew,
                h: eh,
            })
            .map_err(|e| e.to_string())?;
        let right = frame
            .crop_rgb(Rect {
                x: frame.width() * 6 / 10 - ew / 2,
                y: frame.height() * 4 / 10,
                w: ew,
                h: eh,
            })
            .map_err(|e| e.to_string())?;
        let t3 = Instant::now();
        // use the results, so the work cannot be optimised away
        checksum =
            checksum.wrapping_add(rgb[rgb.len() / 2] as u64 + left[0] as u64 + right[0] as u64);
        if i >= frames / 10 {
            read.push((t1 - t0).as_secs_f64() * 1000.0);
            whole.push((t2 - t1).as_secs_f64() * 1000.0);
            crops.push((t3 - t2).as_secs_f64() * 1000.0);
            total.push((t3 - t0).as_secs_f64() * 1000.0);
        }
    }
    println!(
        "{} frames timed (first tenth skipped as warm-up), one thread:",
        total.len()
    );
    line("read one frame from the source", &read);
    line("whole frame to RGB (preview only)", &whole);
    line("two eye-sized regions to RGB", &crops);
    line("all of the above", &total);
    println!("  (checksum {checksum})");
    println!("stages not built yet: face tracking, gaze, eye correction, composite, fake camera");
    Ok(())
}

fn make_clip(mut a: Args) -> Result<(), String> {
    let frames: u64 = a.number("--frames", 60)?;
    let (w, h) = a.size()?;
    if a.0.is_empty() {
        return Err("make-clip needs a file name".into());
    }
    let path = a.0.remove(0);
    a.done()?;
    let mut source = SyntheticSource::new(w, h, PixelFormat::I420, Some(frames));
    let mut out = Y4mWriter::create(&path, w, h, 30).map_err(|e| e.to_string())?;
    while let Some(f) = source.next_frame().map_err(|e| e.to_string())? {
        out.write_i420(f.data()).map_err(|e| e.to_string())?;
    }
    out.finish().map_err(|e| e.to_string())?;
    println!("wrote {frames} frames {w}x{h} to {path}");
    Ok(())
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let command = args.remove(0);
    let result = match command.as_str() {
        "cameras" => Args(args).done().and_then(|()| cameras()),
        "capture" => capture(Args(args)),
        "bench" => bench(Args(args)),
        "make-clip" => make_clip(Args(args)),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command \"{other}\"\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
