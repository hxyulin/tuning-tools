# tuning-studio-app

The shared Tuning Studio backend, exposed as plain Rust methods. The desktop
Tauri host and the VS Code stdio server both use `StudioApp`, so ELF inspection,
connections, live reads, tuning, recording and streaming share one implementation.
This crate does not depend on Tauri or a webview.

## Use it

For a published release:

```toml
[dependencies]
tuning-studio-app = "0.1.0"
```

The Rust library name is **`studio_app`**. Browsing an ELF needs no hardware:

```rust
use studio_app::StudioApp;

fn main() -> Result<(), String> {
    let app = StudioApp::new();
    let opened = app.open_elf("firmware.elf".into())?;
    println!("{} variable roots, {} task slots", opened.roots.len(), opened.tasks.len());
    if let Some(catalog) = opened.catalog {
        println!("{} declared tuning values", catalog.entries.len());
    }
    Ok(())
}
```

## Integrating a frontend

1. Own a `StudioApp`, open an ELF for probe sessions, and present its roots and
   task slots. USB protocol sessions discover their catalog from firmware.
2. Connect with a `ConnectRequest` and a `SessionSink` for sample frames and
   session events. Dispatch blocking methods off your UI/event-loop thread.
3. Use watches for plotted/recorded samples and live inspection methods for
   expanded variables, pointers and task details.
4. Register an `AppEventSink` to receive recording and stream state changes.
   `app_state()` gives the current state to a newly attached UI.
5. Disconnect explicitly when releasing the target. Hiding a frontend need not
   stop the session or its recording.

`record` writes MCAP and exports CSV. `stream` exposes an opt-in TCP service,
bound to loopback by default, with bounded queues per client. Both subscribe to
`Tap` before the UI sink, so a slow UI does not discard their samples. Their own
queues remain bounded and expose drops/errors. Socket workers use cancellable
nonblocking I/O so disconnecting a stalled client cannot wait indefinitely for
its pending writes. The TCP stream is not authenticated;
use its default local binding unless you intend to expose it to your network.

`with_probe_opener` supports custom carriers and deterministic mock tests.
`without_rtt` leaves firmware RTT channels untouched; normal RTT consumption
writes channel read offsets. The server crate additionally enforces a strict
read-only probe wrapper when requested.

Live Watch follows pointers on each poll and preserves exact integer text;
plotted values use floating point. Reads across fields or changing pointers are
not atomic snapshots. See the
[user guide](https://github.com/hxyulin/tuning-tools/blob/main/docs/usage.md)
for inspection limits and the
[stream protocol](https://github.com/hxyulin/tuning-tools/blob/main/docs/stream.md)
for external script integration.

## Development

From the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo test -p tuning-studio-app
```

Tests use mock memory to cover live pointer reads, recording, CSV export and
stream backpressure. Native probe access requires the same system permissions
and libudev development headers on Linux as `tuning-studio-carriers`.

Part of [Tuning Studio](https://github.com/hxyulin/tuning-tools). Host-only (`std`),
MIT licensed. The 0.1 Rust API is evolving.
