//! Uncompressed clip files in the YUV4MPEG2 (".y4m") form.
//!
//! Chosen because it needs no decoder library: a text header, then for each
//! frame the line "FRAME" and the raw I420 bytes. A clip gives the same frames
//! every run, which is what tests and benchmarks need (T-023). The files are
//! large (1280x720 is 1.4 MB a frame), so clips are kept short and out of git.
//!
//! Only 4:2:0 clips are read ("C420", "C420jpeg", "C420mpeg2", "C420paldv", or
//! no C tag, which the format defines as 4:2:0).

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use ec_core::{Frame, PixelFormat};

use crate::{CaptureError, FrameSource};

const MAX_HEADER: usize = 1024;
const MAX_SIDE: u32 = 8192;

pub struct Y4mSource {
    reader: BufReader<File>,
    width: u32,
    height: u32,
    fps: Option<f64>,
    first_frame_at: u64,
    looping: bool,
    seq: u64,
    name: String,
}

/// Read one "\n"-ended line of at most `MAX_HEADER` bytes. `None` at a clean end of file.
fn read_line(r: &mut impl BufRead) -> Result<Option<String>, CaptureError> {
    let mut buf = Vec::new();
    let n = r.take(MAX_HEADER as u64).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(CaptureError::BadClip(
            "header line is cut off or too long".into(),
        ));
    }
    buf.pop();
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| CaptureError::BadClip("header is not text".into()))
}

impl Y4mSource {
    /// `looping`: start again from the first frame when the clip runs out.
    pub fn open(path: impl AsRef<Path>, looping: bool) -> Result<Y4mSource, CaptureError> {
        let path = path.as_ref();
        let mut reader = BufReader::new(File::open(path)?);
        let header =
            read_line(&mut reader)?.ok_or_else(|| CaptureError::BadClip("empty file".into()))?;
        let mut parts = header.split(' ');
        if parts.next() != Some("YUV4MPEG2") {
            return Err(CaptureError::BadClip("not a YUV4MPEG2 file".into()));
        }
        let (mut width, mut height, mut fps) = (None, None, None);
        for p in parts.filter(|p| !p.is_empty()) {
            let (tag, val) = p.split_at(1);
            match tag {
                "W" => width = val.parse::<u32>().ok(),
                "H" => height = val.parse::<u32>().ok(),
                "F" => {
                    fps = val.split_once(':').and_then(|(n, d)| {
                        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
                        (n > 0.0 && d > 0.0).then_some(n / d)
                    })
                }
                "C" if !val.starts_with("420") || val.contains("p1") => {
                    return Err(CaptureError::BadClip(format!(
                        "colour layout C{val}; only 8-bit 4:2:0 is read"
                    )));
                }
                "I" if val != "p" && val != "?" => {
                    return Err(CaptureError::BadClip(
                        "interlaced clips are not read".into(),
                    ));
                }
                _ => {}
            }
        }
        let (width, height) = match (width, height) {
            (Some(w), Some(h))
                if w > 0 && h > 0 && w % 2 == 0 && h % 2 == 0 && w <= MAX_SIDE && h <= MAX_SIDE =>
            {
                (w, h)
            }
            _ => {
                return Err(CaptureError::BadClip(
                    "missing, odd or oversized width/height".into(),
                ))
            }
        };
        let first_frame_at = reader.stream_position()?;
        Ok(Y4mSource {
            reader,
            width,
            height,
            fps,
            first_frame_at,
            looping,
            seq: 0,
            name: path.display().to_string(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl FrameSource for Y4mSource {
    fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        let mut rewound = false;
        loop {
            match read_line(&mut self.reader)? {
                Some(line) if line == "FRAME" || line.starts_with("FRAME ") => break,
                Some(_) => return Err(CaptureError::BadClip("expected a FRAME line".into())),
                None if self.looping && !rewound && self.seq > 0 => {
                    self.reader.seek(SeekFrom::Start(self.first_frame_at))?;
                    rewound = true;
                }
                None => return Ok(None),
            }
        }
        let mut data = vec![0u8; PixelFormat::I420.byte_len(self.width, self.height)];
        self.reader
            .read_exact(&mut data)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::UnexpectedEof => {
                    CaptureError::BadClip("clip ends in the middle of a frame".into())
                }
                _ => CaptureError::Io(e),
            })?;
        let frame = Frame::new(self.width, self.height, PixelFormat::I420, data, self.seq)
            .map_err(|e| CaptureError::BadFrame(e.to_string()))?;
        self.seq += 1;
        Ok(Some(frame))
    }

    fn describe(&self) -> String {
        format!("clip {} {}x{} I420", self.name, self.width, self.height)
    }

    fn nominal_fps(&self) -> Option<f64> {
        self.fps
    }
}

/// Writes I420 frames to a ".y4m" clip.
pub struct Y4mWriter {
    out: BufWriter<File>,
    frame_len: usize,
}

impl Y4mWriter {
    pub fn create(
        path: impl AsRef<Path>,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Y4mWriter, CaptureError> {
        if width == 0
            || height == 0
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || fps == 0
        {
            return Err(CaptureError::BadClip(
                "clip size must be even and non-zero, fps non-zero".into(),
            ));
        }
        let mut out = BufWriter::new(File::create(path)?);
        writeln!(
            out,
            "YUV4MPEG2 W{width} H{height} F{fps}:1 Ip A1:1 C420jpeg"
        )?;
        Ok(Y4mWriter {
            out,
            frame_len: PixelFormat::I420.byte_len(width, height),
        })
    }

    pub fn write_i420(&mut self, i420: &[u8]) -> Result<(), CaptureError> {
        if i420.len() != self.frame_len {
            return Err(CaptureError::BadFrame(format!(
                "frame holds {} bytes, clip needs {}",
                i420.len(),
                self.frame_len
            )));
        }
        self.out.write_all(b"FRAME\n")?;
        self.out.write_all(i420)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), CaptureError> {
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("icontact-test-{}-{name}", std::process::id()))
    }

    fn frame_bytes(k: u8) -> Vec<u8> {
        (0..PixelFormat::I420.byte_len(8, 4))
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(k))
            .collect()
    }

    fn write_clip(path: &Path, n: u8) {
        let mut w = Y4mWriter::create(path, 8, 4, 30).unwrap();
        for k in 0..n {
            w.write_i420(&frame_bytes(k)).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn what_is_written_is_read_back_the_same_every_time() {
        let p = temp("roundtrip.y4m");
        write_clip(&p, 3);
        for _ in 0..2 {
            let mut s = Y4mSource::open(&p, false).unwrap();
            assert_eq!(s.size(), (8, 4));
            assert_eq!(s.nominal_fps(), Some(30.0));
            for k in 0..3u8 {
                let f = s.next_frame().unwrap().unwrap();
                assert_eq!(f.data(), &frame_bytes(k)[..]);
                assert_eq!(f.seq, k as u64);
            }
            assert!(s.next_frame().unwrap().is_none());
        }
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn looping_starts_again_and_keeps_counting() {
        let p = temp("loop.y4m");
        write_clip(&p, 2);
        let mut s = Y4mSource::open(&p, true).unwrap();
        let got: Vec<(u64, u8)> = (0..5)
            .map(|_| s.next_frame().unwrap().unwrap())
            .map(|f| (f.seq, f.data()[0]))
            .collect();
        assert_eq!(got, vec![(0, 0), (1, 1), (2, 0), (3, 1), (4, 0)]);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn a_looping_clip_with_no_frames_ends_instead_of_spinning() {
        let p = temp("empty.y4m");
        write_clip(&p, 0);
        let mut s = Y4mSource::open(&p, true).unwrap();
        assert!(s.next_frame().unwrap().is_none());
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn bad_files_are_refused_with_a_reason() {
        let cases: [(&str, &[u8]); 6] = [
            ("noty4m", b"RIFF....AVI \n"),
            ("nosize", b"YUV4MPEG2 F30:1\n"),
            ("odd", b"YUV4MPEG2 W7 H4 F30:1\n"),
            ("c444", b"YUV4MPEG2 W8 H4 F30:1 C444\n"),
            ("tenbit", b"YUV4MPEG2 W8 H4 F30:1 C420p10\n"),
            ("huge", b"YUV4MPEG2 W99999998 H4 F30:1\n"),
        ];
        for (name, bytes) in cases {
            let p = temp(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(
                matches!(Y4mSource::open(&p, false), Err(CaptureError::BadClip(_))),
                "{name} was accepted"
            );
            std::fs::remove_file(p).unwrap();
        }
    }

    #[test]
    fn a_clip_cut_off_mid_frame_is_an_error_not_a_short_frame() {
        let p = temp("cut.y4m");
        write_clip(&p, 2);
        let len = std::fs::metadata(&p).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(len - 5)
            .unwrap();
        let mut s = Y4mSource::open(&p, false).unwrap();
        assert!(s.next_frame().unwrap().is_some());
        assert!(matches!(s.next_frame(), Err(CaptureError::BadClip(_))));
        std::fs::remove_file(p).unwrap();
    }
}
