//! The app's side: puts finished frames into the shared block.

use std::fs::OpenOptions;
use std::path::Path;
use std::time::Instant;

use ec_core::Frame;
use memmap2::MmapMut;

use crate::layout::{self as l, raw};
use crate::{now_ms, SinkError};

/// The one writer of a shared block. Dropping it tells the readers the app has closed.
pub struct Writer {
    map: MmapMut,
    room: usize,
    started: Instant,
    frames: u64,
}

impl Writer {
    /// Make (or take over) the shared block at `path`, with room for frames of
    /// up to `room` bytes each (see [`l::DEFAULT_ROOM`]).
    ///
    /// A file left by an earlier run is reused, so cameras that still have it
    /// open carry on with the new frames. Fails with [`SinkError::Busy`] if
    /// another app wrote into it within the last second; after a crash, wait
    /// that second and try again.
    pub fn create(path: impl AsRef<Path>, room: usize) -> Result<Writer, SinkError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Not "truncate": a camera may have the old file mapped right now.
        #[allow(clippy::suspicious_open_options)]
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        let wanted = l::block_len(room) as u64;
        let had = file.metadata()?.len();
        if had != wanted {
            // On Windows this fails while a camera has the file open at another
            // size. That only happens when `room` changes between versions.
            file.set_len(wanted)?;
        }
        // SAFETY (for the map): the file is ours to share; everything read from
        // it later goes through raw pointers and is checked, never trusted.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        let base = map.as_mut_ptr();

        // Is there a block from before, and is somebody still feeding it?
        let mut frames = 0;
        // SAFETY: the map is at least HEADER_LEN long and page-aligned.
        unsafe {
            let magic = std::slice::from_raw_parts(base as *const u8, 8) == l::MAGIC;
            if had == wanted && magic && raw::load_u32(base, l::H_VERSION) == l::VERSION {
                let fresh =
                    now_ms().saturating_sub(raw::load_u64(base, l::H_HEARTBEAT)) < l::STALE_MS;
                if raw::load_u32(base, l::H_STATE) == l::STATE_RUNNING && fresh {
                    return Err(SinkError::Busy {
                        pid: raw::load_u32(base, l::H_PID),
                    });
                }
                // carry the count on, so a camera never sees a frame number twice
                frames = raw::load_u64(base, l::H_FRAMES);
            }
            // Fill the header in. The name goes in last: until then no reader
            // takes this for a finished block.
            std::ptr::write_bytes(base, 0, 8);
            raw::store_u32(base, l::H_VERSION, l::VERSION);
            raw::store_u32(base, l::H_HEADER_LEN, l::HEADER_LEN as u32);
            raw::store_u32(base, l::H_SLOT_COUNT, l::SLOT_COUNT as u32);
            raw::store_u32(base, l::H_SLOT_HEADER_LEN, l::SLOT_HEADER_LEN as u32);
            raw::store_u64(base, l::H_ROOM, room as u64);
            raw::store_u32(base, l::H_NEWEST, l::NEWEST_NONE);
            raw::store_u64(base, l::H_FRAMES, frames);
            raw::store_u64(base, l::H_HEARTBEAT, now_ms());
            raw::store_u32(base, l::H_PID, std::process::id());
            raw::store_u32(base, l::H_STATE, l::STATE_RUNNING);
            std::ptr::copy_nonoverlapping(l::MAGIC.as_ptr(), base.add(l::H_MAGIC), 8);
        }
        Ok(Writer {
            map,
            room,
            started: Instant::now(),
            frames,
        })
    }

    /// Put a frame in. It replaces the older of the two frames in the block;
    /// readers see it as the newest from the moment this returns.
    pub fn write(&mut self, frame: &Frame) -> Result<(), SinkError> {
        let data = frame.data();
        if data.len() > self.room {
            return Err(SinkError::TooBig {
                needed: data.len(),
                room: self.room,
            });
        }
        let base = self.map.as_mut_ptr();
        self.frames += 1;
        // microseconds from the app's start to the moment the camera took the frame
        let time = frame
            .captured_at
            .saturating_duration_since(self.started)
            .as_micros() as u64;
        // SAFETY: every offset below is inside the block (the slot starts and
        // lengths come from `layout`, and `data.len() <= room` was checked).
        unsafe {
            // 1. the slot readers are not being sent to
            let slot = if raw::load_u32(base, l::H_NEWEST) == 0 {
                1
            } else {
                0
            };
            let at = l::slot_start(slot, self.room);
            // 2. mark it busy: the next odd number (also right after a crash
            //    that left it odd)
            let busy = (raw::load_u64(base, at + l::S_CHANGE) + 1) | 1;
            raw::store_u64(base, at + l::S_CHANGE, busy);
            // 3. the fields and the picture
            raw::store_u64(base, at + l::S_NUMBER, self.frames);
            raw::store_u64(base, at + l::S_TIME, time);
            raw::store_u32(base, at + l::S_WIDTH, frame.width());
            raw::store_u32(base, at + l::S_HEIGHT, frame.height());
            raw::store_u32(base, at + l::S_FORMAT, l::format_code(frame.format()));
            raw::store_u32(base, at + l::S_LEN, data.len() as u32);
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(at + l::S_PICTURE), data.len());
            // 4. whole again
            raw::store_u64(base, at + l::S_CHANGE, busy + 1);
            // 5. send readers here
            raw::store_u32(base, l::H_NEWEST, slot as u32);
            raw::store_u64(base, l::H_FRAMES, self.frames);
            raw::store_u64(base, l::H_HEARTBEAT, now_ms());
        }
        Ok(())
    }

    /// Say "still here" without a new frame. Call it about twice a second while
    /// there are no frames to send (camera paused), or the readers will take
    /// the app for closed and show their placeholder.
    pub fn beat(&mut self) {
        // SAFETY: a header field of a block we mapped for writing.
        unsafe { raw::store_u64(self.map.as_mut_ptr(), l::H_HEARTBEAT, now_ms()) }
    }

    /// How many frames have gone into this block, earlier runs included.
    pub fn frames_written(&self) -> u64 {
        self.frames
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // SAFETY: a header field of a block we mapped for writing.
        unsafe { raw::store_u32(self.map.as_mut_ptr(), l::H_STATE, l::STATE_CLOSED) }
    }
}
