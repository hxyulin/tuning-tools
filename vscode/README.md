# Tuning Studio

Inspect and tune embedded firmware from VS Code using a debug probe or USB.
Browse ELF symbols, expand live variables, plot telemetry, record samples,
and inspect instrumented Embassy task timing.

## Install

Download the VSIX matching your operating system and architecture from
[GitHub Releases](https://github.com/hxyulin/tuning-tools/releases/tag/v0.1.0).
In VS Code, open Extensions, select **… → Install from VSIX…**, and select it.
Each package includes its native `studio-server`; Rust and Node are not required.
Linux users need libudev and appropriate probe/serial device permissions.

## Get started

1. Open the Tuning Studio Activity Bar view.
2. Select **Open Firmware ELF** and choose your firmware ELF with debug information.
3. Select **Connect Target** and choose a debug probe/chip or USB serial port.
4. Browse symbols and use **Plot Symbol** or **Open Scope** for live inspection.

Live Watch and task inspection require SWD. Task timelines require firmware
instrumentation. A debugger and this extension cannot own the same probe at once.
USB uses the firmware API's declared parameters and telemetry.

The `tuningStudio.serverPath` setting can override the bundled server.
See the [user guide](https://github.com/hxyulin/tuning-tools/blob/main/docs/usage.md)
and [firmware API](https://github.com/hxyulin/tuning-tools/tree/main/crates/tuning-studio-api)
for setup and limitations.
