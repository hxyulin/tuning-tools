# Tuning Tools

Live variable inspection, tuning, telemetry and task timelines for embedded firmware.
Desktop app: Tauri 2 host in Rust, React + TypeScript frontend.

This is the successor to `herkules-tools` and `datavis-rs`. The design,
the reasoning behind it, and the build order are in [FRAME.md](FRAME.md).
Architecture notes on the projects it draws from are in
[docs/references/](docs/references/).

## Status

M3. `studio-dwarf` parses C, C++ and Rust firmware ELFs (symbols, DWARF
types, Rust enums with data, rebuild diff). The app browses statics as a
module tree, attaches to a running target through a debug probe (probe-rs,
no halt, no reset), samples watched numbers on absolute deadlines, plots them,
and shows the firmware's defmt log from RTT. No firmware change is needed.

A firmware that declares a `tuning-studio-api` table gets a Tune tab: its gains
and published state by name, unit and range. The app decodes the table from
the ELF, checks that the target runs that build before it writes, and sends
requests over SWD; the firmware applies them inside its declared range and
step.

The same Tune tab works over the robot's Type-C cable with no probe and no
ELF: pick USB, and the app speaks the tuning-studio-api v1 framed protocol (compatible with `rm-telemetry`), takes
the firmware's tuning lease, lists the table the firmware reports, writes
requests, resets them to defaults, saves them to the robot's flash so they
survive a power cycle, and plots watched values the firmware streams.

Over the probe, a firmware that also serves the protocol on its RTT `control`
and `telemetry` channels gets tuning requests and saves the same way; values
are still sampled from memory. Firmware without those channels is tuned by
writing its table cells directly and cannot save from a probe.

## Develop

```bash
npm install
npm run tauri dev        # desktop app with native probe/serial access
npm run dev              # frontend only, no hardware
cargo test --workspace   # parser tests against fixtures in crates/studio-dwarf/tests
```

Open an ELF at startup instead of through the file dialog:

```bash
TUNING_TOOLS_ELF=path/to/firmware npm run tauri dev
```

Check the parser against a real firmware build:

```bash
STUDIO_DWARF_ELF=path/to/firmware cargo test -p tuning-studio-dwarf -- --ignored
```

Check a probe and board without the app (prints values and log lines):

```bash
cargo run -p tuning-studio-core --example link -- --port /dev/cu.usbmodem101 --rate 500 [--save] <value name>...   # USB link, no app
cargo run -p tuning-studio-core --example watch -- --elf path/to/firmware --list
cargo run -p tuning-studio-core --example watch -- --elf path/to/firmware --chip STM32H723VG <static path>...
cargo run --release -p tuning-studio-carriers --example probe_bench -- STM32H723VG   # raw SWD read latency
```

Requires Rust stable, Node 20+, and the platform Tauri prerequisites.

## Layout

```
src/            React frontend (widgets, layout, data plane consumer)
src-tauri/      Tauri host: commands, event/channel bridge to studio-core
crates/
  studio-dwarf     ELF symbols and DWARF types
  studio-carriers  probe-rs memory access and RTT, mock target
  studio-core      read planner, session thread, stats, sample frames, defmt
docs/           design records and reference notes
```

## VS Code extension

Build the server and extension, or choose **Tuning Studio extension** in this
workspace's Run and Debug menu and press F5 (its pre-launch task builds all three):

```bash
cargo build -p tuning-studio-server
npm run build:webview
npm --prefix vscode install
npm --prefix vscode run build
```

In the Extension Development Host:

- Open **Tuning Studio** in the Activity Bar. The native Target and Symbols views
  can be moved and resized like other VS Code views.
- Use **Open Firmware ELF**, or right-click a firmware file in Explorer. Opening
  the same ELF again reloads a rebuild; disconnect first when changing firmware.
- Choose **Connect Target** to select a debug probe/chip or USB serial port using
  native pickers. Probe defaults come from the workspace's probe-rs `launch.json`.
- Expand symbols and use **Plot Symbol** to add scalar values (or numeric children)
  to the scope. **Go to Source** is available where DWARF includes a source location.
- Open **Tuning Scope** beside your code. Plots, watch readouts and tuning controls
  remain webview content; connection controls, symbol navigation, status and firmware
  logs live in the workbench.
- Click the native status-bar item for target actions, including sample-rate changes.
  Firmware logs appear in **Output → Tuning Studio: Firmware**. Recording commands
  are available in the Command Palette and Target view.

Closing the scope does **not** disconnect the target or stop a recording. Reopening
it attaches to the existing session; chart history starts fresh, while recording
continues. Use **Disconnect Target** to release it. Ending the extension host also
stops the server. A probe-rs debug session still takes priority over probe ownership;
USB remains independent.

Settings: `tuningStudio.serverPath`, `tuningStudio.mockTarget`,
`tuningStudio.sampleRateHz` and `tuningStudio.swdSpeedKhz`. Enable `mockTarget` to
exercise a probe session against an ELF's initialized memory without hardware.
The browser preview is separate and does not render native VS Code views.

Validation: `npm --prefix vscode run typecheck` and
`npm --prefix vscode run harness`. The harness exercises the extension with a mock
server, including probe handoff, recording/export, native commands and scope
reattachment.

## Interface controls

- **Connection settings…** selects the desktop probe/chip or USB port. Expand
  **Advanced** for sampling rate and SWD speed. In VS Code, use native Target Actions.
- **Chart options…** switches between lanes grouped by unit and an overlay.
  The time window, pause/resume and recording stay on the scope toolbar.
- Use a watched value's **⋯** button or right-click its row to show/hide its trace
  or stop watching. Click its unit to change lane grouping.
- **Diagnostics…** shows ELF details, read timing, RTT status and TCP streaming.
  Sample rate, failures, skipped ticks and abnormal core/log states stay visible.

Disconnected desktop sessions rescan devices automatically. Missing remembered devices
stay selected until reattached or explicitly changed. Use **Retry connection** after
an error, or **Reconnect** after disconnecting. VS Code offers **Reconnect Previous
Target** in the Command Palette and Target Actions, retaining the last connection and
sample rate for the current extension session.

Tune values can be filtered by full name and grouped sections can be collapsed.
**Details** contains range, step limit and reset-to-default controls. Unsaved changes,
firmware adjustments and write errors remain visible beside their values.

Recovery checks: `npm --prefix vscode run test:recovery` and the extension harness.

## Live Watch (SWD)

Open **Variables → Live Watch** in the desktop app, or **Live Watch** in the
VS Code scope toolbar. Expand namespaces, structs, arrays and pointers to inspect numeric
fields. Filtering and collapsing groups change the inspected set; **Pause** freezes
readouts, and **W** on a fixed-address field adds it to the existing plot/watch list.

Inspection runs separately from plotted samples, at up to 5 Hz with one request in
flight and a limit of 128 expanded numeric fields. Hidden panels stop polling.
The shared backend uses the datavis-rs-derived `ReadPlan` to coalesce adjacent
fields into bounded memory regions, read by the existing session worker without
halting or resetting the target. This is region coalescing, not the deferred raw
CMSIS-DAP multi-command packet fast path.

Expand a typed pointer's `*` child to follow it. Pointer chains are resolved again
on every poll, with shared pointer reads cached only for that poll and pointee
fields coalesced as usual. Null pointers and failed reads show errors; following
stops after eight dereferences. Void/function pointers cannot be expanded.
Pointer-derived fields are inspection-only: plotting still requires fixed
addresses. Reads occur while the target runs, so a changing pointer and its
pointee are not an atomic snapshot.

Ordinary 64-bit integers display exact decimal text; NaN, Infinity and negative
zero have explicit readouts. Plots still use floating-point numbers. Enum values
show names and Live Watch displays only the active payload (including niches).
Inactive payload reads fail explicitly. Rust borrowed `&str` previews are bounded
to 256 bytes; borrowed slices expose their length and indexed elements. Other
container layouts, such as `String` and `Vec`, remain browsable as DWARF structs.

Arrays and slices have 64-element pages with Previous/Next and an index jump
(press Enter). Slice indices are bounds-checked on every read; use Refresh to
update the page after a slice's length changes. **Pin** adds the selected field
or subtree to a named, persistent watch group. **Pinned groups** switches to that
focused set; unavailable symbols remain saved for their original ELF. Changed
readouts briefly highlight. Inspection remains read-only.
USB targets continue to expose firmware values through Tune. Inspector reads
are separate from recorded plot samples.

Validation: `cargo test -p tuning-studio-app --test live_watch` checks coalescing,
request order, duplicate/missing symbols, changing values, pointer retargeting,
shared/nested pointers, nulls, depth limits and disconnects.


### Inspector test firmware and task history

[Inspector lab](crates/studio-dwarf/tests/fixtures/README.md#inspector_labelf)
provides a runnable STM32H723 binary and repeatable mock/hardware checks for
complex types, pointer chains, large arrays and Embassy task states. Run
`npm --prefix vscode run test:inspector` after building `studio-server`.

The Embassy task detail view shows up to 30 seconds of sampled await-state
history. Select a history block or await point to inspect its type layout; use
**Follow current state** to resume following the live task. Locals expand as a
tree, including structs, arrays and pointers. Inactive-state values are hidden
and are not polled. Task states and locals are separate running-target samples,
so brief transitions can be missed; this is not an execution trace. Hidden pages
stop polling, and narrow panes place task details below the task table.


### Task execution timeline

**Tasks → Execution timeline** reads the firmware's bounded `STUDIO_TASK_TRACE`
ring over SWD. Each task has a lane of completed polls; hover for duration and
wake-to-run latency. Choose a 1 ms–30 s window, zoom and pan through retained
history, or adjust the long-poll threshold (amber bars). Select a bar to freeze
and inspect its duration, wake latency, start and end; **Focus poll** zooms around
that event. **Resume** returns to the latest events. Click a task name to inspect
its locals. See the [timeline investigation guide](docs/task-timeline.md) for
controls, capture limits and troubleshooting.
Missing events are reported and never bridged into invented poll durations.
This works in the desktop app and VS Code; firmware without the buffer retains
the sampled task view. Interrupt time inside a poll is included in its duration.

The included [tuning-studio-trace firmware crate](crates/studio-task-trace/README.md)
is allocation-free and has integration instructions. The inspector lab links it
already. `npm --prefix vscode run test:trace` checks reconstruction, loss, reset,
and latency; `test:inspector -- --hardware` checks actual timestamped polls.

## Packages and releases

See [packaging and manual releases](docs/releases.md) for firmware API integration,
`cargo install`/`cargo binstall`, platform requirements, and the manually triggered
release workflow. The firmware API is [tuning-studio-api](crates/tuning-studio-api/README.md).
