//! Private, opt-in wall-time observations. No policy decisions or Redis calls.
use std::fmt::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

pub const BODY_LIMIT: usize = 384;
pub const FIELDS: [&str; 18] = [
    "v",
    "op",
    "mask",
    "avail_us",
    "diag_us",
    "rt_us",
    "ctx_us",
    "vm_us",
    "run_us",
    "result_us",
    "cdrop_us",
    "rdrop_us",
    "guard_us",
    "total_us",
    "vm_err",
    "polls",
    "hit",
    "flags",
];

#[derive(Default)]
pub struct InterruptObservation {
    calls: AtomicU32,
    overflow: AtomicBool,
    deadline_seen: AtomicBool,
}

impl InterruptObservation {
    // Called only after the original clock comparison; return its saved value.
    pub fn observe(&self, timed_out: bool) -> bool {
        if self.calls.fetch_add(1, Ordering::Relaxed) == u32::MAX {
            self.overflow.store(true, Ordering::Relaxed);
        }
        if timed_out {
            self.deadline_seen.store(true, Ordering::Relaxed);
        }
        timed_out
    }

    fn snapshot(&self) -> (u32, bool, bool) {
        let overflow = self.overflow.load(Ordering::Relaxed);
        (
            if overflow {
                u32::MAX
            } else {
                self.calls.load(Ordering::Relaxed)
            },
            self.deadline_seen.load(Ordering::Relaxed),
            overflow,
        )
    }
}

pub struct Trace {
    stamps: [Option<Instant>; 9],
    deadline: Instant,
    op: u32,
    vm_err: bool,
    pub observation: Arc<InterruptObservation>,
}

impl Trace {
    pub fn new(deadline: Instant, op: u32) -> Self {
        let start = Instant::now();
        Self {
            stamps: [Some(start), None, None, None, None, None, None, None, None],
            deadline,
            op,
            vm_err: false,
            observation: Arc::new(InterruptObservation::default()),
        }
    }

    pub fn stamp(&mut self, phase: usize) {
        self.stamps[phase] = Some(Instant::now());
    }

    pub fn result(&mut self, is_err: bool) {
        self.stamp(5);
        self.vm_err = is_err;
    }

    pub fn record(&self) -> [u32; 18] {
        let (polls, hit, overflow) = self.observation.snapshot();
        let mut flags = if overflow { 4 } else { 0 };
        let mut delta = |from: Option<Instant>, to: Option<Instant>| -> u32 {
            let (Some(from), Some(to)) = (from, to) else {
                return 0;
            };
            let Some(duration) = to.checked_duration_since(from) else {
                flags |= 2;
                return 0;
            };
            u32::try_from(duration.as_micros()).unwrap_or_else(|_| {
                flags |= 1;
                u32::MAX
            })
        };
        let s = &self.stamps;
        // Available policy time is explicitly saturating, even if already expired.
        let available = s[0].map(|start| self.deadline.saturating_duration_since(start));
        let avail_us = available.map_or(0, |d| u32::try_from(d.as_micros()).unwrap_or(u32::MAX));
        let mut values = [
            1,
            self.op,
            s.iter().enumerate().fold(0, |mask, (i, stamp)| {
                mask | (u32::from(stamp.is_some()) << i)
            }),
            avail_us,
            delta(s[0], s[1]),
            delta(s[1], s[2]),
            delta(s[2], s[3]),
            delta(s[3], s[5]),
            delta(s[3], s[4]),
            delta(s[4], s[5]),
            delta(s[5], s[6]),
            delta(s[6], s[7]),
            delta(s[7], s[8]),
            delta(s[0], s[8]),
            u32::from(self.vm_err),
            polls,
            u32::from(hit),
            0,
        ];
        if available.is_some_and(|d| d.as_micros() > u128::from(u32::MAX)) {
            flags |= 1;
        }
        values[17] = flags;
        values
    }
}

pub struct Body {
    bytes: [u8; BODY_LIMIT],
    len: usize,
}

impl Write for Body {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Body {
    pub fn as_str(&self) -> &str {
        // Only static ASCII names and decimal integers are written.
        std::str::from_utf8(&self.bytes[..self.len]).expect("numeric diagnostic is ASCII")
    }
}

pub fn format_record(values: &[u32; 18]) -> Result<Body, fmt::Error> {
    let mut body = Body {
        bytes: [0; BODY_LIMIT],
        len: 0,
    };
    body.write_str("js_phase")?;
    for (field, value) in FIELDS.iter().zip(values) {
        write!(body, " {field}={value}")?;
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formatter_max_width_and_terminators() {
        let body = format_record(&[u32::MAX; 18]).unwrap();
        assert!(body.as_str().len() <= BODY_LIMIT);
        assert!(!body.as_str().contains(['\0', '\n']));
        let mut maximum = [u32::MAX; 18];
        maximum[0] = 1;
        maximum[1] = 2;
        maximum[2] = 511;
        maximum[14] = 1;
        maximum[16] = 1;
        maximum[17] = 7;
        let canonical = format_record(&maximum).unwrap();
        assert_eq!(canonical.as_str().len(), 276);
        let terminated = std::ffi::CString::new(canonical.as_str()).unwrap();
        assert_eq!(terminated.as_bytes_with_nul().len(), 277);
        let mut full = Body {
            bytes: [0; BODY_LIMIT],
            len: 0,
        };
        assert!(full.write_str(&"x".repeat(BODY_LIMIT)).is_ok());
        assert!(full.write_str("x").is_err());
        assert_eq!(full.len, BODY_LIMIT);
    }

    #[test]
    fn counter_wrap_is_sticky_and_preserves_boolean() {
        let observation = InterruptObservation::default();
        assert!(!observation.observe(false));
        assert!(observation.observe(true));
        observation.calls.store(u32::MAX, Ordering::Relaxed);
        assert!(!observation.observe(false));
        assert!(!observation.observe(false));
        assert_eq!(observation.snapshot(), (u32::MAX, true, true));
    }

    #[test]
    fn synthetic_masks_and_direct_vm_interval() {
        let start = Instant::now();
        for (op, error, successful_call) in [(1, false, false), (2, true, false), (2, true, true)] {
            let mut trace = Trace::new(start + Duration::from_millis(25), op);
            trace.stamps =
                std::array::from_fn(|i| Some(start + Duration::from_nanos(i as u64 * 600)));
            trace.vm_err = error;
            if !successful_call {
                trace.stamps[4] = None;
            }
            let values = trace.record();
            assert_eq!(values[2], if successful_call { 511 } else { 495 });
            assert_eq!(values[7], 1); // direct 3-to-5, not sum of two truncated deltas
            assert_eq!((values[8], values[9]), (0, 0));
            assert_eq!(values[14], u32::from(error));
            assert_eq!(values[17], 0);
        }
    }

    #[test]
    fn missing_zero_saturated_and_nonmonotonic_intervals() {
        let start = Instant::now();
        let mut trace = Trace::new(start, 1);
        trace.stamps = [Some(start); 9];
        assert_eq!(trace.record()[7], 0);
        assert_eq!(trace.record()[2], 511);
        trace.stamps[4] = None;
        assert_eq!(trace.record()[2], 495);
        trace.stamps[5] = Some(start + Duration::from_micros(u64::from(u32::MAX) + 1));
        let values = trace.record();
        assert_eq!(values[7], u32::MAX);
        assert_eq!(values[10], 0);
        assert_eq!(values[17], 3);
        trace.stamps = [None; 9];
        assert_eq!(trace.record()[2..14], [0; 12]);
        trace.stamps[0] = Some(start);
        trace.deadline = start + Duration::from_micros(u64::from(u32::MAX) + 1);
        assert_eq!(trace.record()[3], u32::MAX);
        assert_eq!(trace.record()[17], 1);
        trace.deadline = start - Duration::from_micros(1);
        assert_eq!(trace.record()[3], 0);
        // Exercise the public hooks in this host-only binary without logging.
        trace.stamp(1);
        trace.result(true);
        assert_eq!(trace.record()[14], 1);
    }
}
