# SWD task events for Tuning Tools

`tuning-studio-trace` keeps a 1024-event (24 KiB plus header) ring in RAM, exposed as
`STUDIO_TASK_TRACE`. No streaming transport, heap allocation or target halt is
required. The host reads it through the probe it already owns. Events are ready,
poll begin, poll end and task exit. Task IDs are Embassy `TaskHeader` addresses.

Add this crate as a path dependency to firmware and enable Embassy's `trace`
feature. If firmware already defines trace hooks (for example rm-task-stats),
add calls to the existing hooks; **do not define a second set of hook symbols**.
The inspector lab's `rm_task_stats.rs` is a working example:

```rust
// After enabling the DWT counter, before spawning tasks:
tuning_studio_trace::init(CORE_HZ, cortex_m::peripheral::DWT::cycle_count());

// From a periodic interrupt, less than one 32-bit counter wrap apart:
tuning_studio_trace::tick(cortex_m::peripheral::DWT::cycle_count);

// Inside _embassy_trace_task_ready_begin(executor, task):
tuning_studio_trace::record(task, tuning_studio_trace::READY,
    cortex_m::peripheral::DWT::cycle_count);
// Likewise BEGIN in task_exec_begin, END in task_exec_end, EXIT in task_end.
```

The callback reads the clock inside the producer critical section, preventing
interrupt preemption from reordering timestamps. The heartbeat extends 32-bit cycles to 64-bit timestamps even during idle periods.
At 64 MHz it must run more often than every 67 seconds (the lab uses 10 ms).
The counter must keep a constant frequency and must not be reset after init.
For wall-time gaps across sleep, use a counter that continues running in the
target's sleep modes; otherwise the timeline measures only that counter's time.
Reinitialize the trace if the firmware changes its clock. This implementation is
single-core; its critical-section implementation must serialize every producer,
including interrupt callbacks. Do not call it from NMI/HardFault handlers.

The ring's input section is `.data.studio_task_trace`. Place it in probe-visible,
uncached RAM through the board linker/MPU setup. DTCM in the STM32H723 lab needs
no cache management. `init` resets the header and publication stamps; call it
once while no hooks are writing. Other boards must supply a critical-section
implementation and an appropriate monotonic hardware cycle counter.

The ring overwrites old events. Reading slowly or hiding/freezing the UI can
lose events; the UI reports sequence gaps and discards partial poll pairs.
Sequence stamps at both ends of each slot reject torn SWD reads. Poll durations
include time in interrupts that preempt the task. Wake latency begins at the
first captured ready event before a poll; if it was not captured, latency is
unknown. This is a task event timeline, not an instruction trace or an attribution
of interrupt/idle execution. Markers narrower than a pixel are widened for visibility.

For host controls, poll details, capture retention and troubleshooting, see the
[task timeline investigation guide](../../docs/task-timeline.md).

The `large-buffer` feature selects 2048 slots instead of 1024. `custom-section`
places the ring in `.task_trace`; the board must provide a matching linker section
and uncached memory mapping. Call `init` before recording, especially for NOLOAD
sections. These features do not change the v1 wire format.
