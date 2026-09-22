# Tuning Studio

Inspect live embedded variables, plot telemetry, tune parameters, and explore
Embassy tasks from a desktop app or VS Code. Attach over SWD to a running target
without halting or resetting it, or use a firmware-provided USB serial link.

Tuning Studio works with C, C++ and Rust firmware ELFs. It grew out of RoboMaster
tooling, but the firmware API has no robot, board, RTOS or transport dependency.

**Release candidate: 0.1.1.** Registry installation commands below
become available after publication. Until then, build from this repository.
The Rust library APIs and editor protocol are evolving; use matching versions.

## What you can do

- **CAN bus:** configure Damiao adapters, capture/filter raw CAN and CAN FD, record MCAP/CSV, and transmit frames. See [CAN setup](docs/can.md).

- **Live Watch:** expand structs, arrays, enums and pointers; inspect exact 64-bit
  integer values; pin groups of variables and browse large arrays in pages.
- **Scope:** plot numeric fields, group traces by unit, pause and inspect history.
- **Tune:** discover named parameters, ranges and units; request changes that the
  firmware applies; reset defaults and save when firmware implements storage.
- **Tasks:** inspect Embassy await states and locals. Add optional instrumentation
  for a timeline of completed polls and wake-to-run latency.
- **Capture:** record plotted samples to MCAP, export CSV, or stream live data to
  your scripts over TCP. Read firmware defmt logs through RTT.

| Capability | SWD probe + matching ELF | USB serial + firmware API server |
|---|---|---|
| Plot arbitrary numeric statics | Yes | Declared table values only |
| Expand live variables and pointers | Yes | No |
| Named tuning parameters | Requires firmware API table | Yes |
| Save tuning to flash | Requires RTT control server and storage | Requires storage implementation |
| Embassy task inspection | Requires retained task debug information | No |
| Execution timeline | Requires trace instrumentation | No |

[User guide](docs/usage.md) · [Firmware API](crates/tuning-studio-api/README.md) ·
[Task timelines](docs/task-timeline.md) · [TCP stream](docs/stream.md)

## Install

Once 0.1.1 is published, download a native installer from
[GitHub Releases](https://github.com/hxyulin/tuning-tools/releases), or use:

```sh
cargo binstall tuning-studio
# Alternatively, compile the published crate:
cargo install tuning-studio --locked
```

The command is `tuning-studio`. Published source packages include the frontend;
Node.js is only needed when building from the repository.

Release builds target Linux x86-64, Windows x86-64, and macOS Apple Silicon/Intel.
The desktop uses the system webview: WebKitGTK 4.1 on Linux, WebView2 on Windows,
and WebKit on macOS. Source builds also need Rust and the
[Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/), plus
libudev development headers on Linux for probe access. USB devices may need
platform-specific permissions or drivers. Initial native bundles are unsigned;
macOS notarization and Windows signing are not configured.

Install [Tuning Studio from the VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=hxyulin.tuning-studio),
or run:

```sh
code --install-extension hxyulin.tuning-studio
```

VS Code selects the package for your platform and handles updates. The native
server is bundled; Rust and Node are not required. For manual installation,
platform-specific `.vsix` files are also available on the
[release page](https://github.com/hxyulin/tuning-tools/releases/latest).
See the [extension setup](docs/usage.md#vs-code-extension).

## First connection

1. Build your firmware with DWARF debug information and retain the unstripped ELF.
   For Rust release builds, set `debug = 2` and `strip = false` in the firmware's
   `[profile.release]`. Flash that same build using your normal firmware workflow.
2. Launch Tuning Studio and open the ELF. In **Connection settings…**, select the
   probe and target chip, then connect. Release the probe from other debugger
   sessions before connecting.
3. Expand **Variables → Live Watch** to inspect fields. Add fixed-address numeric
   values to the scope with **W**, or select values to watch from the symbol browser.
4. If the firmware declares a tuning table, open **Tune**. For USB, choose the
   serial port instead; a compatible firmware server provides the catalog without
   an ELF or debug probe.

Ordinary SWD variable inspection needs no firmware API. Use
[`tuning-studio-api`](crates/tuning-studio-api/README.md) to expose intentional,
named tuning controls and USB telemetry. Add
[`tuning-studio-trace`](crates/studio-task-trace/README.md) only when you want
instrumented task timing.

## Build and develop

Use current stable Rust, Node.js 24 and the platform prerequisites above.

```sh
git clone https://github.com/hxyulin/tuning-tools.git
cd tuning-tools
npm ci
npm run tauri dev
```

```sh
npm run build                       # frontend and embedded package assets
cargo test --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
npm run tauri -- build               # native release build
```

`npm run dev` runs a browser preview without native hardware access.
`TUNING_TOOLS_ELF=path/to/firmware npm run tauri dev` opens an ELF at startup.
See the [development guide](docs/development.md) for probe checks, fixtures,
VS Code validation and testing against real firmware.

## Crates

| Package | Purpose | Rust import / executable |
|---|---|---|
| [tuning-studio](src-tauri/README.md) | Desktop application | `tuning-studio` executable |
| [tuning-studio-api](crates/tuning-studio-api/README.md) | `no_std` descriptors, protocol server and persistence format | `tuning_studio_api` |
| [tuning-studio-trace](crates/studio-task-trace/README.md) | Optional bounded firmware event ring | `tuning_studio_trace` |
| [tuning-studio-dwarf](crates/studio-dwarf/README.md) | ELF/DWARF parsing and typed variable trees | `studio_dwarf` |
| [tuning-studio-carriers](crates/studio-carriers/README.md) | Probe, RTT, serial and mock transports | `studio_carriers` |
| [tuning-studio-core](crates/studio-core/README.md) | Coalesced reads, sessions, tuning and sample frames | `studio_core` |
| [tuning-studio-app](crates/studio-app/README.md) | Shared desktop/editor backend, recording and streaming | `studio_app` |
| [tuning-studio-server](crates/studio-server/README.md) | Backend over framed stdio for VS Code | `studio-server` executable |

The desktop frontend lives in `src/`, its native host in `src-tauri/`, and the
editor integration in `vscode/`. Both interfaces use the same Rust backend.

## Current limits

Live Watch is read-only and SWD-only, polling at up to 5 Hz with at most 128
expanded numeric fields per request. Pointer-derived fields cannot yet be
plotted. Running-target reads are not atomic snapshots; optimized-away data
cannot be recovered from an ELF. Plots use floating-point values even when
Live Watch preserves exact integer text.

Sample rates depend on the probe, transport, watched regions and host scheduling.
Task timelines require instrumentation, consume RAM, and can lose events when
polling falls behind; detected gaps are shown rather than joined into false
spans. Task polling time includes interrupt preemption. See
[inspection limits](docs/usage.md#live-watch-swd) and
[timeline limits](docs/task-timeline.md) for details.

## Contributing and releases

Bug reports should include the platform, version, transport, target chip and a
small reproducible firmware example when possible. Run the checks above before
submitting changes. [FRAME.md](FRAME.md) and [reference notes](docs/references/)
record the original design and its predecessors, `datavis-rs` and `herkules-tools`.

[Release procedure](docs/releases.md) · [0.1.1 release notes](docs/release-notes/0.1.1.md)

Host crates and the trace recorder are MIT licensed. The extracted firmware API
is MIT OR Apache-2.0; its original notices are included in its package.
