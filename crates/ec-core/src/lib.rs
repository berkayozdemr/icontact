//! Icontact core: the frame type, colour conversion and the hand-off between threads.
//! No operating-system code lives here.

pub mod convert;
pub mod frame;
pub mod slot;
pub mod stats;

pub use frame::{Frame, FrameError, PixelFormat, Rect};
pub use slot::{slot, RecvError, SlotReceiver, SlotSender};
