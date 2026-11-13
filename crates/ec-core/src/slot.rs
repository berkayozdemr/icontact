//! A hand-off between two threads with room for exactly one value (spec §3.3).
//!
//! A newer value replaces an unread older one, so nothing ever queues and delay
//! cannot build up: a slow reader skips frames instead of falling behind.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct State<T> {
    value: Option<T>,
    replaced: u64,
    sender_alive: bool,
    receiver_alive: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

pub struct SlotSender<T> {
    shared: Arc<Shared<T>>,
}

pub struct SlotReceiver<T> {
    shared: Arc<Shared<T>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecvError {
    /// Nothing arrived in time; the sender is still there.
    Timeout,
    /// The sender is gone and the slot is empty.
    Closed,
}

/// The other side was dropped, so the value went nowhere.
#[derive(Debug, PartialEq, Eq)]
pub struct ReceiverGone;

pub fn slot<T>() -> (SlotSender<T>, SlotReceiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            value: None,
            replaced: 0,
            sender_alive: true,
            receiver_alive: true,
        }),
        ready: Condvar::new(),
    });
    (
        SlotSender {
            shared: shared.clone(),
        },
        SlotReceiver { shared },
    )
}

impl<T> SlotSender<T> {
    /// Put a value in the slot. Never waits. `Ok(true)` means an unread value was replaced.
    pub fn send(&self, value: T) -> Result<bool, ReceiverGone> {
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.receiver_alive {
            return Err(ReceiverGone);
        }
        let replaced = s.value.replace(value).is_some();
        if replaced {
            s.replaced += 1;
        }
        drop(s);
        self.shared.ready.notify_one();
        Ok(replaced)
    }
}

impl<T> Drop for SlotSender<T> {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sender_alive = false;
        self.shared.ready.notify_all();
    }
}

impl<T> SlotReceiver<T> {
    /// Take the value if one is waiting.
    pub fn try_recv(&self) -> Option<T> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .value
            .take()
    }

    /// Wait for a value. `None` once the sender is gone and the slot is empty.
    pub fn recv(&self) -> Option<T> {
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = s.value.take() {
                return Some(v);
            }
            if !s.sender_alive {
                return None;
            }
            s = self.shared.ready.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Wait up to `timeout` for a value.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvError> {
        let deadline = Instant::now() + timeout;
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = s.value.take() {
                return Ok(v);
            }
            if !s.sender_alive {
                return Err(RecvError::Closed);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RecvError::Timeout);
            }
            s = self
                .shared
                .ready
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// How many values were replaced before anyone read them.
    pub fn replaced(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replaced
    }
}

impl<T> Drop for SlotReceiver<T> {
    fn drop(&mut self) {
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        s.receiver_alive = false;
        s.value = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn newest_wins_and_replacements_are_counted() {
        let (tx, rx) = slot();
        assert_eq!(tx.send(1), Ok(false));
        assert_eq!(tx.send(2), Ok(true));
        assert_eq!(tx.send(3), Ok(true));
        assert_eq!(rx.try_recv(), Some(3));
        assert_eq!(rx.try_recv(), None);
        assert_eq!(rx.replaced(), 2);
    }

    #[test]
    fn recv_ends_when_sender_goes_but_delivers_the_last_value_first() {
        let (tx, rx) = slot();
        tx.send("last").unwrap();
        drop(tx);
        assert_eq!(rx.recv(), Some("last"));
        assert_eq!(rx.recv(), None);
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(RecvError::Closed)
        );
    }

    #[test]
    fn send_fails_once_receiver_is_gone() {
        let (tx, rx) = slot();
        drop(rx);
        assert_eq!(tx.send(1), Err(ReceiverGone));
    }

    #[test]
    fn timeout_when_nothing_arrives() {
        let (_tx, rx) = slot::<u8>();
        let t = Instant::now();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(30)),
            Err(RecvError::Timeout)
        );
        assert!(t.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn a_waiting_reader_is_woken_by_a_send() {
        let (tx, rx) = slot();
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(Duration::from_millis(20));
        tx.send(7u32).unwrap();
        assert_eq!(reader.join().unwrap(), Some(7));
    }

    #[test]
    fn a_waiting_reader_is_woken_when_the_sender_is_dropped() {
        let (tx, rx) = slot::<u8>();
        let reader = thread::spawn(move || rx.recv());
        thread::sleep(Duration::from_millis(20));
        drop(tx);
        assert_eq!(reader.join().unwrap(), None);
    }

    #[test]
    fn a_slow_reader_only_ever_sees_values_in_order_and_ends_on_the_last() {
        let (tx, rx) = slot();
        let writer = thread::spawn(move || {
            for i in 0..2000u32 {
                tx.send(i).unwrap();
            }
        });
        let mut seen = Vec::new();
        while let Some(v) = rx.recv() {
            seen.push(v);
        }
        writer.join().unwrap();
        assert!(seen.windows(2).all(|p| p[0] < p[1]), "values out of order");
        assert_eq!(*seen.last().unwrap(), 1999);
        assert_eq!(seen.len() as u64 + rx.replaced(), 2000);
    }
}
