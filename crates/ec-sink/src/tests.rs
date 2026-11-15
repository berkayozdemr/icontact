//! Writer and reader together, the way the app and a camera use them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ec_core::{Frame, PixelFormat};

use crate::layout as l;
use crate::{now_ms, Fetch, Reader, SinkError, Writer};

/// A file of its own for each test, removed when the test ends.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> TempFile {
        let file = format!("icontact-test-{}-{name}.bin", std::process::id());
        let path = std::env::temp_dir().join(file);
        let _ = std::fs::remove_file(&path);
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A frame whose every byte is `fill`.
fn flat(width: u32, height: u32, format: PixelFormat, fill: u8) -> Frame {
    let data = vec![fill; format.byte_len(width, height)];
    Frame::new(width, height, format, data, 0).unwrap()
}

fn new_frame(fetch: Fetch) -> Frame {
    match fetch {
        Fetch::New(f) => f,
        other => panic!("expected a new frame, got {other:?}"),
    }
}

#[test]
fn a_frame_comes_out_as_it_went_in_in_every_format() {
    let file = TempFile::new("formats");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    for (i, format) in [
        PixelFormat::Nv12,
        PixelFormat::I420,
        PixelFormat::Yuyv,
        PixelFormat::Rgb24,
        PixelFormat::Bgr24,
    ]
    .into_iter()
    .enumerate()
    {
        // different sizes too: the size may change from one frame to the next
        let (w, h) = (640 + 32 * i as u32, 360 + 18 * 2 * i as u32);
        let data: Vec<u8> = (0..format.byte_len(w, h))
            .map(|k| (k % 253) as u8)
            .collect();
        let sent = Frame::new(w, h, format, data, 0).unwrap();
        writer.write(&sent).unwrap();
        let got = new_frame(reader.fetch().unwrap());
        assert_eq!((got.width(), got.height(), got.format()), (w, h, format));
        assert_eq!(got.data(), sent.data());
        assert_eq!(got.seq, i as u64 + 1); // frames are numbered from 1
    }
}

/// T-060's test: a reader sees the newest frame only.
#[test]
fn a_reader_sees_the_newest_frame_only() {
    let file = TempFile::new("newest");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    assert!(matches!(reader.fetch().unwrap(), Fetch::NothingNew)); // nothing written yet

    for fill in [1, 2, 3] {
        writer
            .write(&flat(64, 48, PixelFormat::Nv12, fill))
            .unwrap();
    }
    // three went in while the reader was away: it gets the third, not the first
    let got = new_frame(reader.fetch().unwrap());
    assert_eq!((got.seq, got.data()[0]), (3, 3));
    assert_eq!(reader.frames_written(), 3);
    // and it is not handed the same frame twice
    assert!(matches!(reader.fetch().unwrap(), Fetch::NothingNew));

    writer.write(&flat(64, 48, PixelFormat::Nv12, 4)).unwrap();
    assert_eq!(new_frame(reader.fetch().unwrap()).seq, 4);
}

#[test]
fn several_readers_each_get_the_frame() {
    let file = TempFile::new("several");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut readers: Vec<Reader> = (0..3).map(|_| Reader::open(&file.0).unwrap()).collect();
    writer.write(&flat(64, 48, PixelFormat::Yuyv, 9)).unwrap();
    for r in &mut readers {
        assert_eq!(new_frame(r.fetch().unwrap()).data()[5], 9);
    }
    // one reader fetching does not use the frame up for a reader that opens later
    let mut late = Reader::open(&file.0).unwrap();
    assert_eq!(new_frame(late.fetch().unwrap()).seq, 1);
}

#[test]
fn a_frame_too_big_for_the_block_is_refused_and_nothing_is_disturbed() {
    let file = TempFile::new("toobig");
    let small = PixelFormat::Nv12.byte_len(64, 48);
    let mut writer = Writer::create(&file.0, small).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    writer.write(&flat(64, 48, PixelFormat::Nv12, 7)).unwrap();
    let refused = writer.write(&flat(128, 96, PixelFormat::Nv12, 8));
    assert!(matches!(refused, Err(SinkError::TooBig { .. })));
    assert_eq!(new_frame(reader.fetch().unwrap()).data()[0], 7);
}

#[test]
fn readers_notice_when_the_app_closes_and_carry_on_when_it_comes_back() {
    let file = TempFile::new("restart");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    writer.write(&flat(64, 48, PixelFormat::Nv12, 1)).unwrap();
    assert_eq!(new_frame(reader.fetch().unwrap()).seq, 1);

    drop(writer); // the app closes
    assert!(matches!(reader.fetch().unwrap(), Fetch::WriterGone));

    // the app starts again; the camera still has the old file open
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    assert!(matches!(reader.fetch().unwrap(), Fetch::NothingNew));
    writer.write(&flat(64, 48, PixelFormat::Nv12, 2)).unwrap();
    let got = new_frame(reader.fetch().unwrap());
    // the numbering carries on, so "have I had this one?" stays a sound question
    assert_eq!((got.seq, got.data()[0]), (2, 2));
}

#[test]
fn an_app_that_stops_without_saying_so_is_noticed_after_a_second() {
    let file = TempFile::new("stale");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    writer.write(&flat(64, 48, PixelFormat::Nv12, 1)).unwrap();
    let now = now_ms();
    // just under a second later: still taken as alive
    assert!(matches!(reader.fetch_at(now + 900).unwrap(), Fetch::New(_)));
    // well over a second with no sign of life (a crash, a freeze)
    assert!(matches!(
        reader.fetch_at(now + 5000).unwrap(),
        Fetch::WriterGone
    ));
    // a paused camera says "still here" without sending frames
    writer.beat();
    assert!(matches!(reader.fetch().unwrap(), Fetch::NothingNew));
}

#[test]
fn a_second_app_cannot_write_into_a_block_that_is_in_use() {
    let file = TempFile::new("busy");
    let _first = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let second = Writer::create(&file.0, l::DEFAULT_ROOM);
    assert!(matches!(second, Err(SinkError::Busy { pid }) if pid == std::process::id()));
}

#[test]
fn files_that_are_not_ours_are_refused() {
    let file = TempFile::new("garbage");
    assert!(matches!(Reader::open(&file.0), Err(SinkError::Io(_)))); // not there yet
    std::fs::write(&file.0, b"short").unwrap();
    assert!(matches!(Reader::open(&file.0), Err(SinkError::NotOurs(_))));
    std::fs::write(&file.0, vec![0x55u8; 4096]).unwrap();
    assert!(matches!(Reader::open(&file.0), Err(SinkError::NotOurs(_))));

    // a real header over a file that is too short for what the header promises
    drop(Writer::create(&file.0, 4096).unwrap());
    let mut bytes = std::fs::read(&file.0).unwrap();
    bytes.truncate(l::HEADER_LEN + 100);
    std::fs::write(&file.0, bytes).unwrap();
    assert!(matches!(Reader::open(&file.0), Err(SinkError::NotOurs(_))));
}

/// The hard case: readers fetching flat out while the writer writes flat out.
/// Every frame is one value repeated, so a frame put together from two
/// different frames ("torn") shows at once.
#[test]
fn readers_racing_the_writer_never_get_a_torn_frame() {
    let file = TempFile::new("race");
    let (w, h) = (640, 360);
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let done = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..3)
        .map(|_| {
            let path = file.0.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                let mut reader = Reader::open(&path).unwrap();
                let (mut got, mut last) = (0u64, 0u64);
                loop {
                    // read the flag first, so one more fetch follows the last write
                    let finished = done.load(Ordering::SeqCst);
                    if let Fetch::New(f) = reader.fetch().unwrap() {
                        let first = f.data()[0];
                        assert!(f.data().iter().all(|b| *b == first), "torn frame {}", f.seq);
                        assert_eq!(
                            first,
                            (f.seq % 251) as u8,
                            "wrong picture for frame {}",
                            f.seq
                        );
                        assert!(f.seq > last, "frame {} after {last}", f.seq);
                        last = f.seq;
                        got += 1;
                    }
                    if finished {
                        return (got, last);
                    }
                }
            })
        })
        .collect();

    let total = 3000u64;
    for n in 1..=total {
        let frame = flat(w, h, PixelFormat::Nv12, (n % 251) as u8);
        writer.write(&frame).unwrap();
    }
    done.store(true, Ordering::SeqCst);
    for r in readers {
        let (got, last) = r.join().unwrap();
        assert!(got > 0);
        assert_eq!(last, total, "every reader ends on the newest frame");
    }
}

#[test]
fn a_patient_reader_waits_for_the_frame_that_is_about_to_arrive() {
    use std::time::{Duration, Instant};
    let file = TempFile::new("patient");
    let mut writer = Writer::create(&file.0, l::DEFAULT_ROOM).unwrap();
    let mut reader = Reader::open(&file.0).unwrap();
    // nothing arrives: it gives up after its patience, not before, and not much later
    let t = Instant::now();
    let nothing = reader.fetch_within(Duration::from_millis(30)).unwrap();
    assert!(matches!(nothing, Fetch::NothingNew));
    assert!(t.elapsed() >= Duration::from_millis(30));
    // a frame arrives 20 ms into a 500 ms wait: it is handed over at once
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        writer.write(&flat(64, 48, PixelFormat::Nv12, 5)).unwrap();
        writer // keep the writer alive until the reader is done
    });
    let t = Instant::now();
    let got = new_frame(reader.fetch_within(Duration::from_millis(500)).unwrap());
    assert_eq!(got.data()[0], 5);
    assert!(
        t.elapsed() < Duration::from_millis(400),
        "{:?}",
        t.elapsed()
    );
    drop(sender.join().unwrap());
    // a closed app is reported at once, without waiting
    let t = Instant::now();
    let gone = reader.fetch_within(Duration::from_millis(500)).unwrap();
    assert!(matches!(gone, Fetch::WriterGone));
    assert!(t.elapsed() < Duration::from_millis(100));
}
