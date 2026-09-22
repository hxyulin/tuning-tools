# tuning-studio-trace

A bounded, allocation-free `no_std` task event recorder for Tuning Studio's SWD
execution timeline. Firmware writes timestamps into the `STUDIO_TASK_TRACE`
RAM ring; the host reads it without halting the target. No byte-stream transport
or dependency on the tuning API is required.

## Add it to firmware

For a published release:

```toml
[dependencies]
tuning-studio-trace = "0.1.1"
```

For unreleased development, use `crates/studio-task-trace` from a checkout, or pin an
immutable Git revision. The Rust import is `tuning_studio_trace`. Firmware must
supply a `critical-section` implementation, native 32-bit atomics, a monotonic
32-bit counter and probe-visible RAM. This implementation supports one core.

| Features | Ring capacity | RAM including header | Input section |
|---|---|---|---|
| Default | 1024 events | 24,596 bytes | `.data.studio_task_trace` |
| `large-buffer` | 2048 events | 49,172 bytes | `.data.studio_task_trace` |
| `custom-section` | Unchanged | Unchanged | `.task_trace` |

Features can be combined and do not change the v1 event format. Account for the
ring in the linker memory budget, including stack space. `custom-section` needs
a board-defined linker section and, on cached systems, suitable MPU/cache setup.

## Embassy integration

Enable Embassy's `trace` feature and call the recorder from its hooks. If
firmware already defines the hooks (for example for CPU statistics), extend
those hooks instead of defining a second set of exported symbols.

For Cortex-M with the DWT cycle counter enabled, the calls look like this:

```rust,ignore
// Once, after clock configuration and before trace producers can write:
tuning_studio_trace::init(CORE_HZ, cortex_m::peripheral::DWT::cycle_count());

// Periodically, even when the traced tasks are idle:
tuning_studio_trace::tick(cortex_m::peripheral::DWT::cycle_count);

// In the task-ready hook; `task` is the TaskHeader address as u32:
tuning_studio_trace::record(
    task,
    tuning_studio_trace::READY,
    cortex_m::peripheral::DWT::cycle_count,
);
```

| Embassy hook | Event |
|---|---|
| `_embassy_trace_task_ready_begin` | `READY` |
| `_embassy_trace_task_exec_begin` | `BEGIN` |
| `_embassy_trace_task_exec_end` | `END` |
| `_embassy_trace_task_end` | `EXIT` |

The host associates task IDs with Embassy `TaskHeader` addresses from the ELF.
Keep the IDs consistent across events. The
[inspector lab hooks](https://github.com/hxyulin/tuning-tools/blob/main/crates/studio-dwarf/tests/fixtures/embassy_tasks/src/rm_task_stats.rs)
show a complete integration. The crate itself does not depend on Embassy.

## Clock and memory requirements

`record` and `tick` read the counter inside the producer critical section so
interrupt preemption cannot reorder timestamps. Call `tick` more often than
one 32-bit wrap: less than `2^32 / counter_hz` seconds (about 67 seconds at
64 MHz or 8.26 seconds at 520 MHz). The lab uses a 10 ms heartbeat.

The counter must keep a constant frequency and must not reset after `init`.
If it stops in sleep, the timeline excludes that stopped time. Use a counter
that continues through sleep when wall-time gaps matter. Reinitialize after a
clock change while no producer is writing.

Place the ring in RAM that the probe sees coherently. The Cortex-M7 data cache
can hide writes in ordinary cached RAM; arrange uncached placement or a suitable
board-specific coherency scheme. Always call `init`, especially for NOLOAD
sections: it resets the header and publication stamps. The critical section
must serialize all writers, including interrupt callbacks. Do not record from
NMI or HardFault handlers.

## Interpreting a capture

Old events are overwritten. Slow reads, hidden/frozen panels, or busy firmware
can cause loss. The UI reports detected sequence gaps and discards incomplete
poll pairs; sequence stamps at both ends of each slot reject torn reads.

Poll durations include interrupts that preempt a task. Wake latency is known
only when its ready event was captured. This does not attribute interrupt/idle
execution and is not an instruction trace or loss-free profiler. A larger ring
uses more RAM and takes longer to read over SWD.

See the [timeline guide](https://github.com/hxyulin/tuning-tools/blob/main/docs/task-timeline.md)
for capture navigation, polling limits and real hardware findings.

## Development and license

From the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo test -p tuning-studio-trace --all-features
cargo check -p tuning-studio-trace --all-features --target thumbv7em-none-eabihf
```

MIT licensed. Pair with a host that understands the v1 trace format.
