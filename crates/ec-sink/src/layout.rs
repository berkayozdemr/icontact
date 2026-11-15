//! The byte layout of the shared block, version 1. This file is the contract:
//! a reader in any language needs nothing else.
//!
//! All numbers are little-endian (every machine we run on is). Offsets are in
//! bytes from the start of the block.
//!
//! ```text
//! HEADER, 128 bytes
//!   0  8 bytes  the text "ICONTACT"   (written last, so a half-made block is never taken for a real one)
//!   8  u32      version = 1
//!  12  u32      header length = 128
//!  16  u32      number of slots = 2
//!  20  u32      slot header length = 64
//!  24  u64      room: the most picture bytes one slot can hold
//!  32  u32      newest: which slot holds the newest whole frame (0 or 1); 0xFFFFFFFF = none yet
//!  36  u32      writer state: 0 = never started, 1 = running, 2 = closed on purpose
//!  40  u64      frames written so far
//!  48  u64      heartbeat: time of the last write, milliseconds since 1970
//!  56  u32      writer's process id
//!  60  ...      zero, kept for later
//!
//! SLOT k (k = 0, 1) starts at 128 + k * (64 + room rounded up to a multiple of 64)
//!   0  u64      change counter: odd while the writer is busy in this slot, even when it is whole
//!   8  u64      frame number (the first frame written is 1)
//!  16  u64      the frame's time: microseconds since the writer started
//!  24  u32      width in pixels
//!  28  u32      height in pixels
//!  32  u32      pixel format: 1 NV12, 2 I420, 3 YUYV, 4 RGB (R,G,B), 5 BGR (B,G,R)
//!  36  u32      picture bytes that follow
//!  40  ...      zero
//!  64  ...      the picture, tightly packed, no padding between rows
//! ```
//!
//! How the writer puts a frame in:
//! 1. pick the slot that is NOT `newest`;
//! 2. add 1 to its change counter (now odd: "busy");
//! 3. write the slot's fields and the picture;
//! 4. add 1 again (even: "whole");
//! 5. set `newest` to this slot, add 1 to `frames written`, set `heartbeat`.
//!
//! How a reader takes the newest frame out:
//! 1. read `newest`; read that slot's change counter; if it is odd, start over;
//! 2. copy the fields and the picture;
//! 3. read the change counter again; if it changed, the copy may be torn: start over.
//!
//! With two slots the writer always works in the slot readers are *not* being
//! sent to, so a reader starts over only if it is slower than a whole frame.
//!
//! When the app is gone: `writer state` is 2, or `heartbeat` is old. The reader
//! then shows its placeholder card (T-062).

use ec_core::PixelFormat;

pub const MAGIC: &[u8; 8] = b"ICONTACT";
pub const VERSION: u32 = 1;
pub const HEADER_LEN: usize = 128;
pub const SLOT_COUNT: usize = 2;
pub const SLOT_HEADER_LEN: usize = 64;

// header fields
pub const H_MAGIC: usize = 0;
pub const H_VERSION: usize = 8;
pub const H_HEADER_LEN: usize = 12;
pub const H_SLOT_COUNT: usize = 16;
pub const H_SLOT_HEADER_LEN: usize = 20;
pub const H_ROOM: usize = 24;
pub const H_NEWEST: usize = 32;
pub const H_STATE: usize = 36;
pub const H_FRAMES: usize = 40;
pub const H_HEARTBEAT: usize = 48;
pub const H_PID: usize = 56;

// slot fields, from the slot's start
pub const S_CHANGE: usize = 0;
pub const S_NUMBER: usize = 8;
pub const S_TIME: usize = 16;
pub const S_WIDTH: usize = 24;
pub const S_HEIGHT: usize = 28;
pub const S_FORMAT: usize = 32;
pub const S_LEN: usize = 36;
pub const S_PICTURE: usize = 64;

pub const NEWEST_NONE: u32 = u32::MAX;
pub const STATE_NEVER: u32 = 0;
pub const STATE_RUNNING: u32 = 1;
pub const STATE_CLOSED: u32 = 2;

/// Room for one 1920 x 1080 frame in the bulkiest format (3 bytes a pixel).
pub const DEFAULT_ROOM: usize = 1920 * 1080 * 3;

/// A heartbeat older than this means the app is gone (or frozen).
pub const STALE_MS: u64 = 1000;

/// One slot's size in the block: its header plus `room`, rounded up to 64.
pub fn slot_len(room: usize) -> usize {
    SLOT_HEADER_LEN + room.div_ceil(64) * 64
}

/// Where slot `k` starts.
pub fn slot_start(k: usize, room: usize) -> usize {
    HEADER_LEN + k * slot_len(room)
}

/// The whole block's size.
pub fn block_len(room: usize) -> usize {
    HEADER_LEN + SLOT_COUNT * slot_len(room)
}

pub fn format_code(f: PixelFormat) -> u32 {
    match f {
        PixelFormat::Nv12 => 1,
        PixelFormat::I420 => 2,
        PixelFormat::Yuyv => 3,
        PixelFormat::Rgb24 => 4,
        PixelFormat::Bgr24 => 5,
    }
}

pub fn format_from_code(code: u32) -> Option<PixelFormat> {
    Some(match code {
        1 => PixelFormat::Nv12,
        2 => PixelFormat::I420,
        3 => PixelFormat::Yuyv,
        4 => PixelFormat::Rgb24,
        5 => PixelFormat::Bgr24,
        _ => return None,
    })
}

/// Reading and writing single numbers in the shared block.
///
/// The block is shared with other processes, so it is only ever touched
/// through a raw pointer, never through a Rust `&[u8]`: a `&` promises that
/// nobody else changes the bytes, and here somebody else does. The numbers
/// both sides change or watch go through "atomics", which make a read or a
/// write happen in one indivisible step (in C#: `Volatile.Read` / `Interlocked`).
pub(crate) mod raw {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// # Safety
    /// `base + at` must lie inside the mapped block and be a multiple of 4
    /// (8 for the u64 ones) from its start. The block starts on a memory page,
    /// and every offset in `layout` is a multiple of 8, so both hold.
    pub unsafe fn load_u32(base: *const u8, at: usize) -> u32 {
        AtomicU32::from_ptr(base.add(at) as *mut u32).load(Ordering::SeqCst)
    }
    /// # Safety
    /// See [`load_u32`].
    pub unsafe fn load_u64(base: *const u8, at: usize) -> u64 {
        AtomicU64::from_ptr(base.add(at) as *mut u64).load(Ordering::SeqCst)
    }
    /// # Safety
    /// See [`load_u32`]; the block must be mapped for writing.
    pub unsafe fn store_u32(base: *mut u8, at: usize, v: u32) {
        AtomicU32::from_ptr(base.add(at) as *mut u32).store(v, Ordering::SeqCst)
    }
    /// # Safety
    /// See [`store_u32`].
    pub unsafe fn store_u64(base: *mut u8, at: usize, v: u64) {
        AtomicU64::from_ptr(base.add(at) as *mut u64).store(v, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_start_on_64_byte_lines_and_do_not_overlap() {
        for room in [1usize, 63, 64, 65, 1_382_400, DEFAULT_ROOM] {
            assert_eq!(slot_start(0, room), 128);
            assert_eq!(slot_start(1, room) % 64, 0);
            assert!(slot_start(1, room) >= slot_start(0, room) + SLOT_HEADER_LEN + room);
            assert!(block_len(room) >= slot_start(1, room) + SLOT_HEADER_LEN + room);
        }
    }

    #[test]
    fn format_codes_go_there_and_back() {
        for f in [
            PixelFormat::Nv12,
            PixelFormat::I420,
            PixelFormat::Yuyv,
            PixelFormat::Rgb24,
            PixelFormat::Bgr24,
        ] {
            assert_eq!(format_from_code(format_code(f)), Some(f));
        }
        assert_eq!(format_from_code(0), None);
        assert_eq!(format_from_code(6), None);
    }
}
