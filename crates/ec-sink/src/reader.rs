//! The fake camera's side: takes the newest frame out of the shared block.
//!
//! The Windows 11 camera will use this code as it is. The Windows 10 one is
//! C++ and follows the same steps from `layout.rs`.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use ec_core::Frame;
use memmap2::Mmap;

use crate::layout::{self as l, raw};
use crate::{now_ms, SinkError};

/// How often to start over when the writer changed the slot under our hands.
/// One retry is already rare (see `layout.rs`); this is only a safety stop.
const TRIES: usize = 5;

/// What a reader got when it asked for the newest frame.
#[derive(Debug)]
pub enum Fetch {
    /// A frame this reader has not had before.
    New(Frame),
    /// The newest frame is the one this reader already has (or there is none yet).
    NothingNew,
    /// The app closed or stopped sending. Show the placeholder.
    WriterGone,
}

/// One reader of a shared block. It opens the block read-only: a reader can
/// never disturb the app or another reader.
pub struct Reader {
    map: Mmap,
    room: usize,
    /// Number of the last frame handed out, 0 for none.
    last: u64,
}

impl Reader {
    /// Open the block at `path`. Fails if the file is not there yet (the app
    /// has never run) or is not a block of our version; try again later.
    pub fn open(path: impl AsRef<Path>) -> Result<Reader, SinkError> {
        let file = File::open(path)?;
        if (file.metadata()?.len() as usize) < l::HEADER_LEN {
            return Err(SinkError::NotOurs("too short"));
        }
        // SAFETY (for the map): see `Writer::create`; nothing in it is trusted.
        let map = unsafe { Mmap::map(&file)? };
        let base = map.as_ptr();
        // SAFETY: the map is at least HEADER_LEN long and page-aligned.
        let room = unsafe {
            let mut magic = [0u8; 8];
            std::ptr::copy_nonoverlapping(base.add(l::H_MAGIC), magic.as_mut_ptr(), 8);
            if &magic != l::MAGIC {
                return Err(SinkError::NotOurs("no ICONTACT mark"));
            }
            if raw::load_u32(base, l::H_VERSION) != l::VERSION {
                return Err(SinkError::NotOurs("another version"));
            }
            if raw::load_u32(base, l::H_HEADER_LEN) as usize != l::HEADER_LEN
                || raw::load_u32(base, l::H_SLOT_COUNT) as usize != l::SLOT_COUNT
                || raw::load_u32(base, l::H_SLOT_HEADER_LEN) as usize != l::SLOT_HEADER_LEN
            {
                return Err(SinkError::NotOurs("unexpected sizes"));
            }
            raw::load_u64(base, l::H_ROOM)
        };
        // The numbers come from a file anyone could have written: check before use.
        let fits = usize::try_from(room)
            .ok()
            .filter(|r| *r <= u32::MAX as usize && l::block_len(*r) <= map.len());
        let Some(room) = fits else {
            return Err(SinkError::NotOurs("the file is shorter than it says"));
        };
        Ok(Reader { map, room, last: 0 })
    }

    /// Take the newest frame, if it is one this reader has not had. Never waits.
    pub fn fetch(&mut self) -> Result<Fetch, SinkError> {
        self.fetch_at(now_ms())
    }

    /// Like [`Reader::fetch`], but if there is nothing new yet, keep looking
    /// (once a millisecond) for up to `patience` before giving up.
    ///
    /// This is what a camera should call. The app and the camera each have
    /// their own 30-a-second clock. When the two tick at almost the same
    /// moment, a camera that looks only once per tick finds "nothing new" on
    /// one tick and has two frames to choose from on the next, again and
    /// again: measured, up to a quarter of all frames were lost that way. Waiting a
    /// little for the frame that is about to arrive loses none.
    pub fn fetch_within(&mut self, patience: Duration) -> Result<Fetch, SinkError> {
        let start = Instant::now();
        loop {
            match self.fetch()? {
                Fetch::NothingNew if start.elapsed() < patience => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                other => return Ok(other),
            }
        }
    }

    /// [`Reader::fetch`] with the clock passed in, so tests can let time pass.
    pub(crate) fn fetch_at(&mut self, now_ms: u64) -> Result<Fetch, SinkError> {
        let base = self.map.as_ptr();
        // SAFETY: header fields; slot offsets are inside the map because
        // `open` checked `block_len(room) <= map.len()`, the slot index is
        // checked to be 0 or 1, and the picture length is checked against `room`.
        unsafe {
            if raw::load_u64(base, l::H_ROOM) != self.room as u64 {
                // a newer app remade the block with another size: open it again
                return Err(SinkError::NotOurs("the block changed size"));
            }
            let stale = now_ms.saturating_sub(raw::load_u64(base, l::H_HEARTBEAT)) >= l::STALE_MS;
            if raw::load_u32(base, l::H_STATE) != l::STATE_RUNNING || stale {
                return Ok(Fetch::WriterGone);
            }
            for _ in 0..TRIES {
                let slot = raw::load_u32(base, l::H_NEWEST);
                if slot == l::NEWEST_NONE {
                    return Ok(Fetch::NothingNew);
                }
                if slot as usize >= l::SLOT_COUNT {
                    return Err(SinkError::NotOurs("slot number out of range"));
                }
                let at = l::slot_start(slot as usize, self.room);
                let change = raw::load_u64(base, at + l::S_CHANGE);
                if change % 2 == 1 {
                    continue; // the writer is in this slot right now
                }
                let number = raw::load_u64(base, at + l::S_NUMBER);
                let width = raw::load_u32(base, at + l::S_WIDTH);
                let height = raw::load_u32(base, at + l::S_HEIGHT);
                let format = raw::load_u32(base, at + l::S_FORMAT);
                let len = raw::load_u32(base, at + l::S_LEN) as usize;
                if raw::load_u64(base, at + l::S_CHANGE) != change {
                    continue; // changed while we read the fields
                }
                if number == self.last {
                    return Ok(Fetch::NothingNew);
                }
                // The fields are whole now, but still only "what the file says".
                let format = l::format_from_code(format)
                    .ok_or(SinkError::NotOurs("unknown pixel format"))?;
                if len > self.room {
                    return Err(SinkError::NotOurs("frame longer than the slot"));
                }
                // Copy the picture out. The writer may start on this slot while
                // we copy; the counter check below catches that and the copy
                // is thrown away. (Copying through raw pointers, because a
                // Rust slice would promise the bytes hold still.)
                let mut picture: Vec<u8> = Vec::with_capacity(len);
                std::ptr::copy_nonoverlapping(
                    base.add(at + l::S_PICTURE),
                    picture.as_mut_ptr(),
                    len,
                );
                picture.set_len(len);
                if raw::load_u64(base, at + l::S_CHANGE) != change {
                    continue; // torn: start over with whatever is newest now
                }
                // `Frame::new` checks that size, format and length agree.
                let frame = Frame::new(width, height, format, picture, number)
                    .map_err(|_| SinkError::NotOurs("frame fields do not agree"))?;
                self.last = number;
                return Ok(Fetch::New(frame));
            }
        }
        // The writer lapped us every time. Keep what we have; ask again next tick.
        Ok(Fetch::NothingNew)
    }

    /// How many frames the app has written so far. Frames written minus frames
    /// fetched is how many this reader let pass.
    pub fn frames_written(&self) -> u64 {
        // SAFETY: a header field inside the map.
        unsafe { raw::load_u64(self.map.as_ptr(), l::H_FRAMES) }
    }
}
