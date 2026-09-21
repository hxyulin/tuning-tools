# Tuning Tools

Live variable watch, tune and telemetry studio for RoboMaster firmware.
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

A firmware that declares an `rm-telemetry` table gets a Tune tab: its gains
and published state by name, unit and range. The app decodes the table from
the ELF, checks that the target runs that build before it writes, and sends
requests over SWD; the firmware applies them inside its declared range and
step.

The same Tune tab works over the robot's Type-C cable with no probe and no
ELF: pick USB, and the app speaks the `rm-telemetry` framed protocol, takes
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
STUDIO_DWARF_ELF=path/to/firmware cargo test -p studio-dwarf -- --ignored
```

Check a probe and board without the app (prints values and log lines):

```bash
cargo run -p studio-core --example link -- --port /dev/cu.usbmodem101 --rate 500 [--save] <value name>...   # USB link, no app
cargo run -p studio-core --example watch -- --elf path/to/firmware --list
cargo run -p studio-core --example watch -- --elf path/to/firmware --chip STM32H723VG <static path>...
cargo run --release -p studio-carriers --example probe_bench -- STM32H723VG   # raw SWD read latency
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
cargo build -p studio-server
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
VS Code scope toolbar. Expand namespaces, structs and arrays to inspect numeric
fields. Filtering and collapsing groups change the inspected set; **Pause** freezes
readouts, and **W** on a field adds it to the existing plot/watch list.

Inspection runs separately from plotted samples, at up to 5 Hz with one request in
flight and a limit of 128 expanded numeric fields. Hidden panels stop polling.
The shared backend uses the datavis-rs-derived `ReadPlan` to coalesce adjacent
fields into bounded memory regions, read by the existing session worker without
halting or resetting the target. This is region coalescing, not the deferred raw
CMSIS-DAP multi-command packet fast path.

This first version is read-only and uses ELF-resolved numeric fields; pointers are
not followed. Values use the existing numeric API (64-bit integers beyond the exact
JavaScript number range may be rounded). USB targets continue to expose firmware
values through Tune. Inspector reads are separate from recorded plot samples.

Validation: `cargo test -p studio-app --test live_watch` checks coalescing,
request order, duplicate/missing symbols, changing values, limits and disconnects.
