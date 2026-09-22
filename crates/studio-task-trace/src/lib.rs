//! Bounded, allocation-free SWD task trace. Call `tick` at least once per
//! 32-bit cycle-counter wrap, and `record` from Embassy's trace hooks.
//! Supports one core; the critical-section implementation must mask all writers.
#![no_std]
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
pub const CAPACITY: usize = 1024;
pub const READY: u32 = 1;
pub const BEGIN: u32 = 2;
pub const END: u32 = 3;
pub const EXIT: u32 = 4;
#[repr(C)]
pub struct Event {
    first: AtomicU32,
    low: AtomicU32,
    high: AtomicU32,
    task: AtomicU32,
    kind: AtomicU32,
    last: AtomicU32,
}
impl Event {
    const fn new() -> Self {
        Self {
            first: AtomicU32::new(0),
            low: AtomicU32::new(0),
            high: AtomicU32::new(0),
            task: AtomicU32::new(0),
            kind: AtomicU32::new(0),
            last: AtomicU32::new(0),
        }
    }
}
#[repr(C)]
pub struct Trace {
    magic: AtomicU32,
    version: AtomicU32,
    capacity: AtomicU32,
    hz: AtomicU32,
    head: AtomicU32,
    events: [Event; CAPACITY],
}
#[used]
#[no_mangle]
#[cfg_attr(target_os = "none", link_section = ".data.studio_task_trace")]
pub static STUDIO_TASK_TRACE: Trace = Trace {
    magic: AtomicU32::new(0x53545452),
    version: AtomicU32::new(1),
    capacity: AtomicU32::new(CAPACITY as u32),
    hz: AtomicU32::new(0),
    head: AtomicU32::new(0),
    events: [const { Event::new() }; CAPACITY],
};
static PREVIOUS: AtomicU32 = AtomicU32::new(0);
static HIGH: AtomicU32 = AtomicU32::new(0);
/// Initialize before starting the executor. The table must reside in probe-visible RAM.
pub fn init(hz: u32, cycle: u32) {
    critical_section::with(|_| {
        STUDIO_TASK_TRACE.magic.store(0, Relaxed);
        for event in &STUDIO_TASK_TRACE.events {
            event.first.store(0, Relaxed);
            event.last.store(0, Relaxed);
        }
        STUDIO_TASK_TRACE.version.store(1, Relaxed);
        STUDIO_TASK_TRACE.capacity.store(CAPACITY as u32, Relaxed);
        STUDIO_TASK_TRACE.hz.store(hz, Relaxed);
        STUDIO_TASK_TRACE.head.store(0, Relaxed);
        PREVIOUS.store(cycle, Relaxed);
        HIGH.store(0, Relaxed);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        STUDIO_TASK_TRACE.magic.store(0x53545452, Relaxed);
    });
}
fn extend(cycle: u32) -> u32 {
    if cycle < PREVIOUS.load(Relaxed) {
        HIGH.fetch_add(1, Relaxed);
    }
    PREVIOUS.store(cycle, Relaxed);
    HIGH.load(Relaxed)
}
/// Periodic heartbeat extends the cycle clock even while all tasks sleep.
pub fn tick(read_cycle: impl FnOnce() -> u32) {
    critical_section::with(|_| {
        extend(read_cycle());
    });
}
pub fn record(task: u32, kind: u32, read_cycle: impl FnOnce() -> u32) {
    critical_section::with(|_| {
        if STUDIO_TASK_TRACE.hz.load(Relaxed) == 0 {
            return;
        }
        let cycle = read_cycle();
        let high = extend(cycle);
        let seq = STUDIO_TASK_TRACE.head.load(Relaxed).wrapping_add(1);
        let e = &STUDIO_TASK_TRACE.events[seq.wrapping_sub(1) as usize % CAPACITY];
        e.first.store(seq ^ 0x80000000, Relaxed);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        e.low.store(cycle, Relaxed);
        e.high.store(high, Relaxed);
        e.task.store(task, Relaxed);
        e.kind.store(kind, Relaxed);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        e.last.store(seq, Relaxed);
        e.first.store(seq, Relaxed);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        STUDIO_TASK_TRACE.head.store(seq, Relaxed);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heartbeat_extends_wrap_and_ring_overwrites_in_sequence() {
        init(64_000_000, u32::MAX - 10);
        tick(|| u32::MAX - 1);
        tick(|| 3);
        record(42, BEGIN, || 8);
        let first = &STUDIO_TASK_TRACE.events[0];
        assert_eq!(first.low.load(Relaxed), 8);
        assert_eq!(first.high.load(Relaxed), 1);
        assert_eq!(first.first.load(Relaxed), first.last.load(Relaxed));
        for i in 0..CAPACITY as u32 {
            record(42, END, || 9 + i);
        }
        assert_eq!(STUDIO_TASK_TRACE.head.load(Relaxed), CAPACITY as u32 + 1);
        assert_eq!(first.first.load(Relaxed), CAPACITY as u32 + 1);
        init(64_000_000, 0);
        assert_eq!(first.first.load(Relaxed), 0);
        assert_eq!(STUDIO_TASK_TRACE.head.load(Relaxed), 0);
    }
}
