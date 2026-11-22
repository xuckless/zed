//! The display server's input clock read next to GPUI's, so an event's millisecond time becomes a GPUI `Instant`
//! by its age, as `TouchEvent::time_stamp` wants. Wayland compositors (Weston, Mutter, KWin and wlroots stamp input
//! from libinput), Xorg and Xwayland (`GetTimeInMillis`) all stamp events with `CLOCK_MONOTONIC` in milliseconds,
//! wrapping at `u32`, so both backends read it here.

use std::time::{Duration, Instant};

/// An age past this, either way, means the server stamps on another clock (a remote display, a test client's
/// made-up times): no time stamp, so GPUI times the touch when it dispatches it.
const MAX_EVENT_AGE_MILLIS: u32 = 10_000;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock {
    pub(crate) now: Instant,
    /// `CLOCK_MONOTONIC` at `now` in milliseconds, wrapped to `u32` as event times wrap.
    pub(crate) monotonic_millis: u32,
}

impl Clock {
    pub(crate) fn read() -> Self {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `time` is a valid, writable timespec, and CLOCK_MONOTONIC exists on every Linux and FreeBSD.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
        let millis = (time.tv_sec as u64)
            .wrapping_mul(1000)
            .wrapping_add(time.tv_nsec as u64 / 1_000_000);
        Self {
            now: Instant::now(),
            // Truncation is the point: servers send the low 32 bits.
            monotonic_millis: millis as u32,
        }
    }

    /// When an event stamped `time` happened, on GPUI's clock (Flutter's `PointerEvent.timeStamp`). A stamp a
    /// little after the clock read counts as now; one more than [`MAX_EVENT_AGE_MILLIS`] away is `None`.
    pub(crate) fn at(self, time: u32) -> Option<Instant> {
        // The wrapping difference read as signed, so an age across the u32 wrap stays small.
        let age = self.monotonic_millis.wrapping_sub(time) as i32;
        if age.unsigned_abs() > MAX_EVENT_AGE_MILLIS {
            return None;
        }
        self.now
            .checked_sub(Duration::from_millis(age.max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Extend: one row per kind of event time a backend can send.
    #[test]
    fn event_time_stamps() {
        // (row name, CLOCK_MONOTONIC ms at the clock read, the event's time, its age in ms, or None when untimed)
        let rows: [(&str, u32, u32, Option<u64>); 8] = [
            ("on time", 5_000, 5_000, Some(0)),
            ("a frame old", 5_000, 4_984, Some(16)),
            ("after the clock read is now", 1_000, 1_003, Some(0)),
            ("across the u32 wrap", 10, u32::MAX - 5, Some(16)),
            (
                "ten seconds old is still timed",
                20_000,
                10_000,
                Some(10_000),
            ),
            (
                "older than ten seconds is another clock",
                20_001,
                10_000,
                None,
            ),
            ("a foreign clock far behind", 100_000, 30_000, None),
            ("a foreign clock ahead", 30_000, 100_000, None),
        ];
        let now = Instant::now() + Duration::from_secs(60);
        for (name, monotonic_millis, time, age) in rows {
            let clock = Clock {
                now,
                monotonic_millis,
            };
            let expected = age.map(|age| now - Duration::from_millis(age));
            assert_eq!(clock.at(time), expected, "{name}");
        }
    }
}
