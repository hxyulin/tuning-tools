//! Per-task CPU time for embassy, kept where a host tool can read it over SWD.
//!
//! With the `embassy` feature this crate implements embassy-executor's
//! `trace` hooks: around every task poll it reads the Cortex-M cycle counter
//! and adds the poll's cycles to that task's slot in [`RM_TASK_STATS`]. The
//! host samples the slots twice a second and turns the differences into CPU
//! share, polls per second and the longest recent poll. Nothing is sent; the
//! cost on the target is a few dozen cycles per poll.
//!
//! ```toml
//! embassy-executor = { workspace = true, features = ["trace", ...] }
//! rm-task-stats = { workspace = true, features = ["embassy"] }
//! ```
//!
//! ```ignore
//! rm_task_stats::init(bsp::SYSCLK_HZ);
//! ```
//!
//! # What is counted
//!
//! A slot counts the cycles between the executor starting a poll of the task
//! and the poll returning. Interrupts that fire during a poll are counted in
//! it. Time outside every poll (interrupts between polls, the executor, and
//! sleep) is what the host shows as the rest. The hooks assume one executor;
//! an `InterruptExecutor` preempting a poll would be counted in the preempted
//! task.
//!
//! # Layout contract
//!
//! The host decodes [`TaskStats`] and [`Slot`] by field name from DWARF, and
//! matches a slot's `task` to the task pool that holds that `TaskHeader`.
//! Renaming or retyping a field, or changing what one means, is a format
//! change and bumps [`STATS_VERSION`].

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// `TaskStats::magic`: `RMTS` in little-endian byte order.
pub const STATS_MAGIC: u32 = u32::from_le_bytes(*b"RMTS");
pub const STATS_VERSION: u32 = 1;
/// Tasks tracked at once; later spawns only count in `untracked`
pub const MAX_TASKS: usize = 32;

/// The counters the host reads, found by this name in the ELF. On target
/// builds with the `embassy` feature the hooks fill it.
///
/// On target it lives in `.probe`, which the board linker scripts keep out of
/// the D-cache: a probe reads RAM behind the cache, and these lines are hot.
/// That section is not loaded, so [`stats`] resets it before first use.
#[cfg_attr(target_arch = "arm", unsafe(link_section = ".probe"))]
#[used]
pub static RM_TASK_STATS: TaskStats<MAX_TASKS> = TaskStats::new();

/// One task's counters. Cycle counts wrap; the host differences them.
#[repr(C)]
#[derive(Debug)]
pub struct Slot {
    /// The task's `TaskHeader` address, as embassy's trace hooks name it;
    /// 0 while the slot is free
    task: AtomicU32,
    /// Polls finished
    polls: AtomicU32,
    /// Cycles spent in those polls, wrapping
    cycles: AtomicU32,
    /// Longest poll since the task was spawned, in cycles
    max_cycles: AtomicU32,
    /// Longest poll in the current window, and in the one before it
    peak: AtomicU32,
    last_peak: AtomicU32,
    /// Cycle count when the current window began
    window_start: AtomicU32,
}

impl Slot {
    const fn new() -> Self {
        Self {
            task: AtomicU32::new(0),
            polls: AtomicU32::new(0),
            cycles: AtomicU32::new(0),
            max_cycles: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            last_peak: AtomicU32::new(0),
            window_start: AtomicU32::new(0),
        }
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct TaskStats<const N: usize> {
    /// [`STATS_MAGIC`] once reset; anything else is uninitialized memory
    magic: AtomicU32,
    version: AtomicU32,
    /// Cycle counter frequency; 0 until [`init`]
    clock_hz: AtomicU32,
    /// Tasks spawned while every slot was taken
    untracked: AtomicU32,
    /// The poll in progress: its slot plus one (0 for none), and its start
    running: AtomicU32,
    started: AtomicU32,
    slots: [Slot; N],
}

impl<const N: usize> Default for TaskStats<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TaskStats<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            magic: AtomicU32::new(STATS_MAGIC),
            version: AtomicU32::new(STATS_VERSION),
            clock_hz: AtomicU32::new(0),
            untracked: AtomicU32::new(0),
            running: AtomicU32::new(0),
            started: AtomicU32::new(0),
            slots: [const { Slot::new() }; N],
        }
    }

    /// Forget every task and counter.
    pub fn reset(&self) {
        self.magic.store(0, Relaxed);
        for counter in [&self.clock_hz, &self.untracked, &self.running, &self.started] {
            counter.store(0, Relaxed);
        }
        for slot in &self.slots {
            for counter in [
                &slot.task,
                &slot.polls,
                &slot.cycles,
                &slot.max_cycles,
                &slot.peak,
                &slot.last_peak,
                &slot.window_start,
            ] {
                counter.store(0, Relaxed);
            }
        }
        self.version.store(STATS_VERSION, Relaxed);
        self.magic.store(STATS_MAGIC, Relaxed);
    }

    pub fn set_clock(&self, hz: u32) {
        self.clock_hz.store(hz, Relaxed);
    }

    fn find(&self, task: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.task.load(Relaxed) == task)
    }

    /// A task was spawned: give it a slot with zeroed counters.
    pub fn task_new(&self, task: u32, now: u32) {
        if task == 0 {
            return;
        }
        let Some(i) = self.find(task).or_else(|| self.find(0)) else {
            self.untracked.fetch_add(1, Relaxed);
            return;
        };
        let slot = &self.slots[i];
        for counter in [
            &slot.polls,
            &slot.cycles,
            &slot.max_cycles,
            &slot.peak,
            &slot.last_peak,
        ] {
            counter.store(0, Relaxed);
        }
        slot.window_start.store(now, Relaxed);
        slot.task.store(task, Relaxed);
    }

    /// A task finished: free its slot.
    pub fn task_end(&self, task: u32) {
        if let Some(i) = self.find(task).filter(|_| task != 0) {
            self.slots[i].task.store(0, Relaxed);
        }
    }

    /// The executor is about to poll `task`.
    pub fn poll_begin(&self, task: u32, now: u32) {
        let slot = self.find(task).filter(|_| task != 0).map_or(0, |i| i + 1);
        self.started.store(now, Relaxed);
        self.running.store(slot as u32, Relaxed);
    }

    /// The poll begun by [`Self::poll_begin`] returned.
    pub fn poll_end(&self, now: u32) {
        let running = self.running.swap(0, Relaxed) as usize;
        let Some(slot) = running.checked_sub(1).and_then(|i| self.slots.get(i)) else {
            return;
        };
        let spent = now.wrapping_sub(self.started.load(Relaxed));
        slot.polls.fetch_add(1, Relaxed);
        slot.cycles.fetch_add(spent, Relaxed);
        slot.max_cycles.fetch_max(spent, Relaxed);

        // Windows of about a second, so a startup poll does not hide a slow
        // one now. Checked only when the task polls; the host reads the larger
        // of the two peaks.
        let window = self.clock_hz.load(Relaxed);
        if window != 0 && now.wrapping_sub(slot.window_start.load(Relaxed)) >= window {
            slot.last_peak.store(slot.peak.swap(0, Relaxed), Relaxed);
            slot.window_start.store(now, Relaxed);
        }
        slot.peak.fetch_max(spent, Relaxed);
    }
}

/// Start the cycle counter and record its frequency, the core clock.
///
/// Call once, early in `main`. Polls before this count as zero cycles.
#[cfg(target_arch = "arm")]
pub fn init(core_hz: u32) {
    cycles::enable();
    stats().set_clock(core_hz);
}

/// Set once `.probe` holds a reset table. A plain `.bss` static: startup
/// zeroes it before the executor exists, so before any hook runs.
#[cfg(target_arch = "arm")]
static READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// [`RM_TASK_STATS`], reset on first use since startup does not initialize it.
#[cfg(target_arch = "arm")]
#[inline(always)]
fn stats() -> &'static TaskStats<MAX_TASKS> {
    if !READY.load(Relaxed) {
        RM_TASK_STATS.reset();
        READY.store(true, Relaxed);
    }
    &RM_TASK_STATS
}

#[cfg(target_arch = "arm")]
mod cycles {
    const DEMCR: *mut u32 = 0xE000_EDFC as *mut u32;
    const DWT_CTRL: *mut u32 = 0xE000_1000 as *mut u32;
    const DWT_CYCCNT: *const u32 = 0xE000_1004 as *const u32;
    /// Cortex-M7 locks the DWT against software writes until this is written
    const DWT_LAR: *mut u32 = 0xE000_1FB0 as *mut u32;
    const TRCENA: u32 = 1 << 24;
    const CYCCNTENA: u32 = 1;

    pub fn enable() {
        // SAFETY: DEMCR, DWT_LAR and DWT_CTRL are architectural registers
        // present on every ARMv7-M core; setting TRCENA and CYCCNTENA only
        // starts the trace unit and its cycle counter. The unlock write is
        // ignored on cores without a lock.
        unsafe {
            DEMCR.write_volatile(DEMCR.read_volatile() | TRCENA);
            DWT_LAR.write_volatile(0xC5AC_CE55);
            DWT_CTRL.write_volatile(DWT_CTRL.read_volatile() | CYCCNTENA);
        }
    }

    #[inline(always)]
    pub fn now() -> u32 {
        // SAFETY: DWT_CYCCNT is an architectural, always-readable register on
        // ARMv7-M; reading it has no side effects.
        unsafe { DWT_CYCCNT.read_volatile() }
    }
}

/// embassy-executor's trace hooks, declared `extern "Rust"` in
/// `embassy_executor::raw::trace`. Ids are addresses: the executor's and the
/// task's `TaskHeader`.
#[cfg(all(feature = "embassy", target_arch = "arm"))]
mod hooks {
    use super::{cycles, stats};

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_task_new(_executor: u32, task: u32) {
        stats().task_new(task, cycles::now());
    }

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_task_end(_executor: u32, task: u32) {
        #[cfg(feature = "trace")] studio_task_trace::record(task, studio_task_trace::EXIT, cycles::now);
        stats().task_end(task);
    }

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_task_exec_begin(_executor: u32, task: u32) {
        #[cfg(feature = "trace")] studio_task_trace::record(task, studio_task_trace::BEGIN, cycles::now);
        stats().poll_begin(task, cycles::now());
    }

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_task_exec_end(_executor: u32, _task: u32) {
        #[cfg(feature = "trace")] studio_task_trace::record(_task, studio_task_trace::END, cycles::now);
        stats().poll_end(cycles::now());
    }

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_task_ready_begin(_executor: u32, _task: u32) {
        #[cfg(feature = "trace")] studio_task_trace::record(_task, studio_task_trace::READY, cycles::now);
    }

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_poll_start(_executor: u32) {}

    #[unsafe(no_mangle)]
    pub fn _embassy_trace_executor_idle(_executor: u32) {}
}
