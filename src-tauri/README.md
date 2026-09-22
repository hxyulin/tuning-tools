# Tuning Studio

Live embedded variable inspection, tuning, telemetry and task timelines in a
desktop app. Connect to running firmware over SWD, or to a compatible firmware
server over USB serial. The application shares its Rust backend with the
Tuning Studio VS Code extension.

## Install

For a published release:

```sh
cargo binstall tuning-studio
# Or build the published source package:
cargo install tuning-studio --locked
```

Launch with `tuning-studio`. Native installers are distributed through
[GitHub Releases](https://github.com/hxyulin/tuning-tools/releases): Linux x86-64
(deb/AppImage), Windows x86-64 (NSIS), and macOS Apple Silicon/Intel (DMG).
Initial bundles are unsigned; signing and notarization are not configured.

Published source packages contain prebuilt frontend assets, so end users do not
need Node.js. Source builds require Rust and the
[Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/), plus
libudev development headers on Linux. Binary installs still need the platform
webview runtime: WebKitGTK 4.1 on Linux, WebView2 on Windows, or system WebKit on
macOS. Probe/serial devices may need platform-specific permissions or drivers.

## First use

1. Open the unstripped ELF matching the flashed firmware.
2. Select the debug probe and chip in **Connection settings…**, then connect.
3. Browse **Variables → Live Watch**, expand fields and pointers, and plot
   fixed-address numeric values.
4. Use **Tune** for firmware-declared parameters, **Tasks** for Embassy task
   inspection, and recording to capture plotted samples as MCAP or CSV.

For USB, select the serial port instead. Firmware must implement the
`tuning-studio-api` server; its catalog works without an ELF. Ordinary SWD
inspection needs no API integration. Execution timelines require the optional
`tuning-studio-trace` recorder in firmware.

Live reads do not halt/reset the target and are not atomic snapshots. Live Watch
is SWD-only; pointer-derived fields are not plottable yet. Traces use a bounded
ring and can lose events if the host reads too slowly. The UI reports detected
loss. USB exposes only firmware-declared values.

## Develop from source

From the repository root:

```sh
npm ci
npm run tauri dev
# Build frontend assets before direct Cargo builds:
npm run build
cargo build -p tuning-studio
```

The desktop uses Rust/Tauri 2 and React/TypeScript. Its package includes a host
library for Tauri wiring; reusable integrations should depend on
`tuning-studio-app` instead.

[Project and crate map](https://github.com/hxyulin/tuning-tools) ·
[User guide](https://github.com/hxyulin/tuning-tools/blob/main/docs/usage.md) ·
[Release procedure](https://github.com/hxyulin/tuning-tools/blob/main/docs/releases.md)

MIT licensed.

Release 0.1.1: see the [release notes](https://github.com/hxyulin/tuning-tools/blob/main/docs/release-notes/0.1.1.md).
