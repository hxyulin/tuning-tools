# CAN capture

Open **CAN bus** in the desktop app or VS Code webview. CAN runs independently
of the firmware/ELF/probe session and keeps capturing when you switch tabs.

1. Scan adapters and select the connected adapter and model. For USB2CANFD,
   choose channel 0. The public SDK has no structured model/serial getter, so
   model selection is manual and adapter numbers are valid until the next scan.
2. Set the arbitration bitrate and sample point. Enable CAN FD when appropriate
   and set its data bitrate/sample point. Values are in bit/s and fractions
   (for example, `1000000` and `0.75`). Defaults are suggestions, not detection.
3. Connect. Match the bus settings and physical termination. The SDK does not
   expose silent/listen-only mode, so capture may acknowledge bus traffic.
4. Filter by an exact hexadecimal CAN ID or RX/TX/error direction. Pause/clear
   only affects the view. The view retains 2,000 frames, backed by 10,000 frames
   of worker history; view skips are distinct from capture queue drops.
5. Record to a **new** MCAP or CSV file. Recording is independent of UI filters,
   pauses, and tab visibility. Disconnect finalizes the recording. Existing
   files are never overwritten. CAN recording is separate from watch recording.
6. Use **Send once** for an explicit single transmission. FD payload lengths
   must be 0–8, 12, 16, 20, 24, 32, 48, or 64 bytes. Classic remote requests
   currently use DLC 0. TX counters reflect SDK callbacks, not button presses.

Disconnect before changing timing. Advanced mode overrides the bitrate/sample
point controls with the SDK's 8-bit segment, SJW and prescaler fields. The actual
bitrate depends on the adapter clock. Supported timings still require hardware
validation; a successful SDK call does not prove communication with another node.

MCAP uses topic `can/frames` with JSON messages and channel configuration metadata.
Each frame keeps the SDK timestamp as a decimal string with **unknown units and
timebase**, plus host arrival Unix nanoseconds (also a string to preserve precision).
Host arrival timestamps are used for MCAP time; no firmware-clock alignment is
claimed. CSV uses the same frame information with hexadecimal IDs and payloads.
A bounded 65,536-frame SDK callback queue protects memory and exposes overflow.
Drop counts cover this host queue, not losses within the adapter or vendor SDK.

## SDK packaging

The integration dynamically loads the Damiao Device SDK 1.1 native library. It
has no Python or .NET requirement. Builds without the SDK keep all other Studio
features available and show an actionable error when CAN discovery is requested.
The SDK library path can be set in the panel or through
`TUNING_STUDIO_DMCAN_LIBRARY`. The desktop app also finds bundled resources;
the server finds libraries next to its executable.

The native SDK runs in an isolated child of the Studio executable. This contains
vendor crashes and prevents its stdout diagnostics from corrupting server IPC.
Scanning opens USB handles immediately; disconnect releases them. No additional
application is required. SDK requests time out after 10 seconds if it stops responding.

For a local self-contained macOS desktop build using an existing SDK download:

```sh
python3 scripts/prepare-can-sdk.py /path/to/dm-device-sdk-master
npm run tauri build -- --debug --bundles app --config .can-sdk.local/tauri.json
```

The generated configuration stages host-platform libraries in a git-ignored
`.can-sdk.local` directory and bundles them as app resources. For development:

```sh
TUNING_STUDIO_DMCAN_LIBRARY=/absolute/path/to/libdm_device.dylib npm run tauri dev
```

The supplied SDK has macOS Intel/ARM64, Linux x64/ARM64 and Windows x64 binaries.
Windows may require a USB driver and Linux USB permissions; bundling the library
does not configure either. Windows staging includes the supplied libusb DLL.
Before public distribution, establish the vendor library redistribution terms;
the download inspected for this integration contains no license file. Public
release automation has not been changed to distribute vendor binaries.

The server refuses CAN connection/transmission under `--read-only` because the
SDK cannot guarantee silent capture. `--mock` never opens physical CAN hardware.

## Hardware acceptance check

With a USB2CANFD and a known-good bus, verify discovery, configuration, standard
and extended RX, CAN FD RX when supported by the bus, explicit TX and echo, CSV
and MCAP recordings, stop/reconnect, unplug/replug, and sustained-load drop
counts. No automatic test transmits on physical hardware. Device removal status
and error details depend on callbacks from the vendor SDK; if an unplugged
adapter remains shown as connected, disconnect and rescan.

## DM-MC02 heartbeat/echo firmware

The `feat/tuning` firmware worktree at
`../../Embedded/rm-embedded-rs-tuning` includes `dm-mc02-studio-can-test`.
Build/flash it there with `cargo xtask flash dm-mc02-studio-can-test --release`.
It leaves actuator rails off and runs both CAN1 and CAN2 at classic 1 Mbit/s.
It does not enumerate a USB CDC port; the board USB connection can still supply
power. Ensure the transceiver 5 V supply is connected, as documented in the
firmware's bench findings.

Connect the USB2CANFD to **one** board CAN connector, using labeled CANH/CANL
and common ground, with 120-ohm termination at each bus end. In Studio select
USB2CANFD, **adapter channel 0**, arbitration `1000000`, sample point `0.75`,
CAN FD off, advanced timing off. Scan, select, and connect. Reset the board once
after connecting if it was running without an acknowledging peer.

| Board connector | Heartbeat at 10 Hz | Reply to standard data ID `700` |
|---|---|---|
| CAN1 | `701` | `711`, identical payload |
| CAN2 | `702` | `712`, identical payload |

Send ID `700`, data `11 22 33 44 55 66 77 88`, with Extended/FD/BRS/RTR off.
The **RX reply** proves the round trip; the adapter's TX callback alone does not.
Record a short MCAP/CSV capture to test logging. Disconnect before moving the
adapter to the other connector, then reconnect/reset and repeat.

Bench status: firmware flash/read-back verification and RTT startup/advancing
heartbeat tasks passed on the attached DM-MC02. USB2CANFD classic 1 Mbit/s RX passed, with advancing `702` heartbeats on the
connector reported as CAN1. Connector mapping needs confirmation. Test ID `700`
was accepted by the SDK but no echo or TX callback was observed, so transmission
and round-trip operation remain unverified. An unconnected port fills its TX queue;
that is expected and does not establish a problem on the other port.
