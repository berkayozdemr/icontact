//! Icontact command-line tool: list cameras, time a camera, benchmark the
//! pipeline stages (T-025), run the face tracker on a clip, and correct a clip (T-056).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ec_capture::{
    list_cameras, request_camera_permission, CameraSource, FrameSource, SyntheticSource, Y4mSource,
    Y4mWriter,
};
use ec_core::stats::Timings;
use ec_core::{PixelFormat, Rect};
use ec_vision::{Calibration, FaceTracker, Level, Pipeline};

const USAGE: &str = "\
ec-cli cameras
    List the cameras the system offers.
ec-cli capture [--camera N] [--frames N] [--size WxH] [--save FILE.ppm]
    Open a camera, report what it delivers and how evenly frames arrive.
    --save writes the last frame as a picture, to check colours by eye.
ec-cli bench (--clip FILE.y4m | --synthetic nv12|i420|yuyv|rgb|bgr) [--frames N] [--size WxH]
    Time each pipeline stage. --size applies to --synthetic only.
ec-cli track --clip FILE.y4m [--frames N] [--threads N] [--raw] [--points FILE.csv]
    Run the face tracker over a clip and time it. --raw switches smoothing off.
    --points writes every frame's 478 points (x,y) as CSV.
ec-cli correct --clip IN.y4m [--out OUT.y4m] [--stats FILE.csv] [--frames N] [--threads N]
               [--strength 0..1] [--turn UP,RIGHT] [--camera-above DEG] [--slowdown X]
    Correct the eyes in a clip and time every stage. --out writes the corrected clip,
    --stats one line per frame (gaze, strength, turn, level).
    --turn asks for a fixed turn in degrees instead of estimating the gaze.
    --camera-above is how far above the screen's middle the camera sits (default 10).
    --slowdown 5 behaves like a processor 5 times slower, to watch the ladder step down.
ec-cli eye-dump --clip IN.y4m --frame N --turn UP,RIGHT --out DIR
    Write everything about both eyes of one frame (patch, model answer, before, after)
    as raw files, for training/train/check_app.py to compare with Python.
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

    /// True if `--name` was given; removed from the list.
    fn flag(&mut self, name: &str) -> bool {
        match self.0.iter().position(|a| a == name) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
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

    /// `--name UP,RIGHT`: two numbers of degrees.
    fn angles(&mut self, name: &str) -> Result<Option<[f32; 2]>, String> {
        match self.value(name)? {
            None => Ok(None),
            Some(v) => v
                .split_once(',')
                .and_then(|(a, b)| Some([a.trim().parse().ok()?, b.trim().parse().ok()?]))
                .map(Some)
                .ok_or(format!("{name}: \"{v}\" must look like 8,-3 (up,right)")),
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
    println!("face tracking and eye correction are timed by: ec-cli track, ec-cli correct\nstage not built yet: fake camera");
    Ok(())
}

/// Run the face tracker over a clip: timings, how often the face was held, and
/// optionally every point of every frame as CSV (for comparing with the Python reference).
fn track(mut a: Args) -> Result<(), String> {
    let clip = a.value("--clip")?.ok_or("track needs --clip FILE.y4m")?;
    let frames: usize = a.number("--frames", usize::MAX)?;
    let threads: usize = a.number("--threads", 1)?;
    let points_path = a.value("--points")?;
    let raw = a.flag("--raw");
    a.done()?;
    let mut source = Y4mSource::open(&clip, false).map_err(|e| e.to_string())?;
    let fps = source.nominal_fps().unwrap_or(30.0);
    println!("source: {}", source.describe());
    let mut tracker = FaceTracker::new(threads).map_err(|e| e.to_string())?;
    tracker.smoothing = !raw;
    let mut csv = match &points_path {
        Some(p) => Some(std::io::BufWriter::new(
            std::fs::File::create(p).map_err(|e| format!("{p}: {e}"))?,
        )),
        None => None,
    };
    let (mut detect, mut mesh, mut total) =
        (Timings::default(), Timings::default(), Timings::default());
    let (mut seen, mut with_face, mut redetected) = (0usize, 0usize, 0usize);
    let (mut open, mut yaw, mut pitch) =
        (Timings::default(), Timings::default(), Timings::default());
    while seen < frames {
        let Some(frame) = source.next_frame().map_err(|e| e.to_string())? else {
            break;
        };
        let t = Instant::now();
        let face = tracker
            .track(&frame, seen as f64 / fps)
            .map_err(|e| e.to_string())?;
        total.push(t.elapsed().as_secs_f64() * 1000.0);
        if let Some(d) = tracker.last_times.detect_ms {
            detect.push(d);
        }
        if let Some(face) = &face {
            mesh.push(tracker.last_times.mesh_ms);
            with_face += 1;
            redetected += face.redetected as usize;
            open.push(((face.eye_open[0] + face.eye_open[1]) / 2.0) as f64);
            yaw.push(face.head.yaw as f64);
            pitch.push(face.head.pitch as f64);
        }
        if let Some(out) = csv.as_mut() {
            use std::io::Write;
            let mut row = format!("{seen}");
            for p in face.iter().flat_map(|f| f.points.iter()) {
                row += &format!(",{:.2},{:.2}", p[0], p[1]);
            }
            writeln!(out, "{row}").map_err(|e| e.to_string())?;
        }
        seen += 1;
    }
    println!(
        "{seen} frames, face in {with_face}, detector ran {} times ({redetected} found a face), {threads} thread(s), smoothing {}:",
        detect.len(),
        if raw { "off" } else { "on" }
    );
    line("face detector (when it runs)", &detect);
    line("cut square + face mesh model", &mesh);
    line("whole tracker, per frame", &total);
    let range = |t: &Timings| match (t.percentile(0.0), t.median(), t.percentile(1.0)) {
        (Some(a), Some(b), Some(c)) => format!("min {a:.2}  median {b:.2}  max {c:.2}"),
        _ => "-".into(),
    };
    println!("  eye openness (both eyes' mean)      {}", range(&open));
    println!("  head yaw, degrees                   {}", range(&yaw));
    println!("  head pitch, degrees                 {}", range(&pitch));
    if let Some(p) = points_path {
        println!("points written to {p}");
    }
    Ok(())
}

/// Correct a clip (T-056): every frame through the same [`Pipeline`] the app uses.
/// Prints where the time went (T-057) and how the gate and the ladder behaved.
fn correct(mut a: Args) -> Result<(), String> {
    let clip = a.value("--clip")?.ok_or("correct needs --clip FILE.y4m")?;
    let out_path = a.value("--out")?;
    let stats_path = a.value("--stats")?;
    let frames: usize = a.number("--frames", usize::MAX)?;
    let threads: usize = a.number("--threads", 1)?;
    let strength: f32 = a.number("--strength", ec_vision::DEFAULT_STRENGTH)?;
    let camera_above: f32 = a.number("--camera-above", ec_vision::gaze::DEFAULT_CAMERA_ABOVE)?;
    let slowdown: f32 = a.number("--slowdown", 1.0)?;
    let fixed_turn = a.angles("--turn")?;
    a.done()?;

    let mut source = Y4mSource::open(&clip, false).map_err(|e| e.to_string())?;
    let fps = source.nominal_fps().unwrap_or(30.0);
    let (w, h) = source.size();
    println!("source: {}", source.describe());
    let mut pipeline = Pipeline::new(threads).map_err(|e| e.to_string())?;
    pipeline.slowdown = slowdown;
    pipeline.corrector.settings.strength = strength;
    pipeline.corrector.settings.fixed_turn = fixed_turn;
    pipeline.corrector.settings.calibration = Calibration::Assumed { camera_above };
    let mut writer = match &out_path {
        Some(p) => Some(Y4mWriter::create(p, w, h, fps.round() as u32).map_err(|e| e.to_string())?),
        None => None,
    };
    let mut stats = match &stats_path {
        Some(p) => {
            use std::io::Write;
            let mut f =
                std::io::BufWriter::new(std::fs::File::create(p).map_err(|e| format!("{p}: {e}"))?);
            writeln!(f, "frame,face,level,repeated,gaze_up,gaze_right,strength_left,strength_right,turn_up_left,turn_right_left,turn_up_right,turn_right_right,changed_pixels,total_ms").map_err(|e| e.to_string())?;
            Some(f)
        }
        None => None,
    };

    // one collection of timings per stage
    let mut t_track = Timings::default();
    let mut t_gaze = Timings::default();
    let mut t_cut = Timings::default();
    let mut t_model = Timings::default();
    let mut t_apply = Timings::default();
    let mut t_total = Timings::default();
    let (mut seen, mut with_face, mut corrected, mut repeated) = (0usize, 0usize, 0usize, 0usize);
    let mut at_level = [0usize; 4];
    let mut level_changes = 0usize;
    let mut last_level = Level::Full;
    let mut last_strength: Option<[f32; 2]> = None;
    let mut biggest_jump = 0.0f32;
    let (mut gaze_up, mut gaze_right, mut turn_size) =
        (Timings::default(), Timings::default(), Timings::default());

    // in thousandths of the maps' unit, so the numbers are readable
    let mut map_change = Timings::default();
    while seen < frames {
        let Some(frame) = source.next_frame().map_err(|e| e.to_string())? else {
            break;
        };
        let p = pipeline
            .process(frame, seen as f64 / fps)
            .map_err(|e| e.to_string())?;
        at_level[p.level as usize] += 1;
        level_changes += (p.level != last_level) as usize;
        last_level = p.level;
        repeated += p.repeated as usize;
        with_face += p.face.is_some() as usize;
        if !p.repeated {
            t_total.push(p.total_ms);
            if let Some(ms) = p.track_ms {
                t_track.push(ms);
            }
        }
        if let Some(c) = &p.correction {
            corrected += (c.changed_pixels > 0) as usize;
            t_gaze.push(c.times.gaze_ms);
            if c.times.model_ms > 0.0 {
                t_cut.push(c.times.cut_ms);
                t_model.push(c.times.model_ms);
            }
            if c.changed_pixels > 0 {
                t_apply.push(c.times.apply_ms);
            }
            if let Some(g) = c.gaze {
                gaze_up.push(g[0] as f64);
                gaze_right.push(g[1] as f64);
            }
            turn_size.push(c.turn[0][0].hypot(c.turn[0][1]) as f64);
            if c.map_change[0] > 0.0 {
                map_change.push(c.map_change[0] as f64 * 1000.0);
            }
            // The gate's promise: the strength never jumps between two frames.
            if let Some(prev) = last_strength {
                for (now, before) in c.strength.iter().zip(prev) {
                    biggest_jump = biggest_jump.max((now - before).abs());
                }
            }
            last_strength = Some(c.strength);
        } else if !p.repeated {
            last_strength = None;
        }
        if let Some(f) = stats.as_mut() {
            use std::io::Write;
            let c = p.correction.unwrap_or_default();
            let g = c.gaze.unwrap_or([f32::NAN; 2]);
            writeln!(
                f,
                "{seen},{},{},{},{:.2},{:.2},{:.3},{:.3},{:.2},{:.2},{:.2},{:.2},{},{:.3}",
                p.face.is_some() as u8,
                p.level as u8,
                p.repeated as u8,
                g[0],
                g[1],
                c.strength[0],
                c.strength[1],
                c.turn[0][0],
                c.turn[0][1],
                c.turn[1][0],
                c.turn[1][1],
                c.changed_pixels,
                p.total_ms
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(out) = writer.as_mut() {
            if p.frame.format() != PixelFormat::I420 {
                return Err("the clip writer needs I420 frames".into());
            }
            out.write_i420(p.frame.data()).map_err(|e| e.to_string())?;
        }
        seen += 1;
    }
    if let Some(out) = writer {
        out.finish().map_err(|e| e.to_string())?;
    }

    println!(
        "{seen} frames, face in {with_face}, eyes changed in {corrected}, {threads} thread(s), strength {strength}{}:",
        match fixed_turn {
            Some(t) => format!(", fixed turn up {:+} right {:+}", t[0], t[1]),
            None => format!(", camera taken to be {camera_above} degrees above the usual gaze"),
        }
    );
    line("find the face", &t_track);
    line("gaze + gate", &t_gaze);
    line("cut both eyes for the model", &t_cut);
    line("warp model, both eyes", &t_model);
    line("move pixels + write back", &t_apply);
    line("whole frame", &t_total);
    let range = |t: &Timings| match (t.percentile(0.02), t.median(), t.percentile(0.98)) {
        (Some(a), Some(b), Some(c)) => format!("2% {a:+.1}  median {b:+.1}  98% {c:+.1}"),
        _ => "-".into(),
    };
    println!("  gaze up, degrees from the camera    {}", range(&gaze_up));
    println!(
        "  gaze right, degrees                 {}",
        range(&gaze_right)
    );
    println!(
        "  turn applied, degrees (left eye)    {}",
        range(&turn_size)
    );
    println!(
        "  eye model answer, change per frame   {}",
        range(&map_change)
    );
    println!(
        "  biggest strength change between two frames: {biggest_jump:.3} (a fade allows {:.3})",
        1.0 / fps as f32 / ec_vision::gate::FADE_SECONDS
    );
    println!(
        "  ladder: full {}, tracking every 2nd {}, eye model every 2nd {}, 15 a second {} ({repeated} pictures repeated); level changed {level_changes} times",
        at_level[0], at_level[1], at_level[2], at_level[3]
    );
    if let Some(p) = out_path {
        println!("corrected clip written to {p}");
    }
    if let Some(p) = stats_path {
        println!("per-frame numbers written to {p}");
    }
    Ok(())
}

/// Write everything about both eyes of one frame as raw files, so a Python
/// script can redo the same steps and compare (T-051).
fn eye_dump(mut a: Args) -> Result<(), String> {
    let clip = a.value("--clip")?.ok_or("eye-dump needs --clip FILE.y4m")?;
    let wanted: usize = a.number("--frame", 30)?;
    let turn = a
        .angles("--turn")?
        .ok_or("eye-dump needs --turn UP,RIGHT")?;
    let dir = a.value("--out")?.ok_or("eye-dump needs --out DIR")?;
    a.done()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
    let mut source = Y4mSource::open(&clip, false).map_err(|e| e.to_string())?;
    let fps = source.nominal_fps().unwrap_or(30.0);
    let mut pipeline = Pipeline::new(1).map_err(|e| e.to_string())?;
    pipeline.corrector.settings.fixed_turn = Some(turn);
    for i in 0..=wanted {
        let frame = source
            .next_frame()
            .map_err(|e| e.to_string())?
            .ok_or("the clip is shorter than --frame")?;
        pipeline.corrector.keep_debug = i == wanted;
        pipeline
            .process(frame, i as f64 / fps)
            .map_err(|e| e.to_string())?;
    }
    if pipeline.corrector.debug.is_empty() {
        return Err(format!(
            "no eye was corrected in frame {wanted} (no face, or the eyes were shut)"
        ));
    }
    // f32 values as raw little-endian bytes: numpy reads them with np.fromfile(..., "<f4")
    let floats = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
    for eye in &pipeline.corrector.debug {
        let name = format!("{dir}/{}", format!("{:?}", eye.side).to_lowercase());
        let save = |suffix: &str, bytes: &[u8]| {
            std::fs::write(format!("{name}{suffix}"), bytes)
                .map_err(|e| format!("{name}{suffix}: {e}"))
        };
        let b = &eye.eye_box;
        save(
            ".txt",
            format!(
                "frame {wanted}\norigin {} {}\nacross {} {}\ndown {} {}\nrect {} {} {} {}\nmodel_turn {} {}\nmirrored {}\n",
                b.origin[0], b.origin[1], b.across[0], b.across[1], b.down[0], b.down[1],
                eye.rect.x, eye.rect.y, eye.rect.w, eye.rect.h,
                eye.model_turn[0], eye.model_turn[1],
                eye.side.mirrored() as u8
            )
            .as_bytes(),
        )?;
        save("_before.rgb", &eye.before)?;
        save("_after.rgb", &eye.after)?;
        save("_input.f32", &floats(&eye.input))?;
        save("_flow.f32", &floats(&eye.maps.flow))?;
        save("_bright.f32", &floats(&eye.maps.bright))?;
        println!("wrote {name}.txt and five data files");
    }
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
        "track" => track(Args(args)),
        "correct" => correct(Args(args)),
        "eye-dump" => eye_dump(Args(args)),
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
