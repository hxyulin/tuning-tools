# Investigating Embassy task timing

The desktop app and VS Code share the task timeline. Open the firmware ELF,
connect an SWD probe, and open **Logs & tasks → Tasks → Execution timeline**
(desktop) or the scope's **Tasks → Execution timeline** view (VS Code).
Firmware must include the [trace buffer and hooks](../crates/studio-task-trace/README.md).
The [inspector lab](../crates/studio-dwarf/tests/fixtures/README.md#inspector_labelf)
already includes these hooks; a matching ELF is essential for task names and addresses.

## Find and inspect a slow poll

1. Let the timeline collect events. Each lane represents a task; each bar is one
   completed call to poll its future. Gray bars are ordinary polls. Amber bars
   meet or exceed **Long poll above**, expressed in milliseconds.
2. Select a bar. Capture freezes and a detail panel shows duration, wake-to-run
   latency, start and end. Use **Focus poll** to zoom around that poll.
3. Use **Zoom in**, **Zoom out**, or the window selector (1 ms through 30 s).
   Zoom buttons center on the selected poll, or the viewport midpoint when
   nothing is selected. **Earlier** and **Later** pan by half a window.
4. Click a task name to return to its sampled state and locals. Use **Resume**
   in the timeline to clear the selection and return to the newest events.

Navigation freezes capture so the data does not move or expire while you inspect
it. Selecting a new window while live continues following the latest event.
Keyboard users can focus a lane and use Left/Right to pan and +/− to zoom.
Poll bars are also focusable; Enter or Space selects one.

The lane summary reports the maximum full poll duration and known wake latency
among polls intersecting the visible window. A poll clipped by a window edge
still shows its full duration in details. Very short polls get a minimum-width
marker so they remain visible; use the numeric duration for precise comparisons.

## What the measurements mean

- **Duration:** time from the poll-begin hook to the poll-end hook. Interrupts
  that preempt the task are included; this is not exclusive task CPU time.
- **Wake-to-run:** time from the first captured ready event to poll begin.
  “Not captured” means no matching ready event was available, not zero latency.
- **Start and end:** milliseconds on the firmware trace clock, not host wall time.
  The clock's behavior during sleep determines whether sleep contributes to gaps.
- **Empty space:** no completed poll was reconstructed there. It does not identify
  idle time, interrupt execution, or the reason a task was waiting.

The separate **Tasks** view samples await states and locals. It can miss brief
state transitions; the timeline records hook events instead. Neither view halts
the target. Live Watch reads, task states and trace snapshots are not one atomic
capture.

## Capture limits and recovery

The firmware ring holds 1024 events (24 KiB plus header). The host reads it about
every 500 ms while visible and running, retaining at most 30 seconds and 20,000
completed polls. Busy firmware may overwrite records between reads. The warning
counts detected sequence gaps; polls spanning those gaps are discarded rather
than assigned a misleading duration. Events already overwritten before the first
read cannot be counted.

**Freeze**, navigation and hidden views stop trace reads, not firmware execution.
Resuming can therefore report overwritten events. The retained view is temporary:
leaving the timeline, disconnecting, or changing the ELF may clear it. Trace export
and offline reopening are not available yet; plot MCAP recordings do not include
these task events.

If there are no bars:

- **No STUDIO_TASK_TRACE buffer:** integrate the trace crate and use the matching
  ELF. Ordinary sampled task inspection remains available without it.
- **Clock not initialized:** call trace initialization before spawning tasks and
  provide a periodic counter heartbeat as described in the firmware guide.
- **Read failed:** displayed data is stale. Check the connection and whether the
  ring is in probe-visible, uncached RAM.
- **Buffer found but empty:** check that the Embassy hooks call the recorder and
  tasks reach both poll-begin and poll-end. An unfinished poll has no bar.

## Developer checks

Run `npm --prefix vscode run test:trace` for event reconstruction, loss/reset,
latency and navigation-boundary checks. Build both shared frontends with
`npm run build` and `npm run build:webview`. The inspector lab's
`npm --prefix vscode run test:inspector -- --hardware` checks actual timestamped
polls on the connected STM32H723; see the fixture guide before flashing it.

## Production firmware bench check

On 2026-09-22, `balance-infantry-chassis` from the RM Embedded workspace was
flashed to the STM32H723 DM-MC02 and checked with its matching debug ELF. All 11
task instances were running, task statistics were readable, and 234 scalar task
local fields were read successfully. The desktop UI showed changing robot ticks,
IMU readings, and expanded `InfantryApp` counters. The main task ran at about
1,000 polls/s; the IMU task ran at about 14,000 polls/s during this bench check.
These observations validate inspection, not the robot's control behavior.

That build has `rm-task-stats` but no `STUDIO_TASK_TRACE` ring. The timeline
correctly reports missing instrumentation, while sampled tasks and locals work.
For a workload this busy, simply adding all trace hooks is insufficient for
continuous capture: even two events per IMU poll exceed the 1024-slot ring within
about 37 ms, before counting ready events or other tasks. Task filtering, a larger
buffer with a measured memory budget, or a faster capture transport is needed
before expecting loss-free production timelines with 500 ms host reads.

A subsequent optional `timeline` build instruments only the main control task
and adds a 100 ms clock heartbeat (12 running task instances total). Its 2048-slot
ring is placed in a dedicated, MPU-configured non-cacheable AXI SRAM region at
`0x24040000`, outside the application's DTCM stack space. The firmware workspace's
`robots/balance-infantry/chassis/README.md` documents the feature and build command.
The first instrumented run in DTCM failed during boot; the AXI placement booted
and captured 9,619 completed polls across clock wraps in a hardware check.

This probe took 532–618 ms per full ring read in that check and detected 9,695
missed events. The host now counts read time toward its 500 ms polling interval,
with one request in flight, but this remains a lossy capture. Other task lanes
are filtered out, not evidence of inactivity. Incremental reads or faster
transport remain necessary for continuous, loss-free production tracing.
