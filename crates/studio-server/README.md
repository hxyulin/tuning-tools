# tuning-studio-server

The Tuning Studio backend over framed stdin/stdout, used by the VS Code extension.
It provides the same inspection, acquisition, tuning, recording and streaming
operations as the desktop app without requiring Tauri or a webview.

The package is `tuning-studio-server`; the executable is **`studio-server`**.

## Install and run

For a published release:

```sh
cargo install tuning-studio-server --locked
studio-server --version
```

Or build from the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo build -p tuning-studio-server
```

Set VS Code's `tuningStudio.serverPath` to the installed executable if the
extension cannot find it. See the
[extension setup](https://github.com/hxyulin/tuning-tools/blob/main/docs/usage.md#vs-code-extension)
for building the extension itself. This server is normally launched by its client;
it waits for framed requests and is not an interactive shell.

| Option | Effect |
|---|---|
| `--version` | Print the package version and exit |
| `--mock` | Use an in-memory probe target initialized from the opened ELF |
| `--read-only` | Refuse target-memory writes and disable RTT consumption |

`--mock --read-only` is useful for frontend development without hardware. A mock
image does not execute firmware or simulate peripheral behavior. Normal hardware
connections require USB permissions/drivers and exclusive ownership of the probe.
Linux source builds need libudev development headers.

## Client protocol

Each message is `u32 little-endian length | u8 kind | payload`; length includes
kind and payload but excludes the four-byte length field.

- `J`: UTF-8 JSON. Requests have `id`, `method` and `params`. Responses carry
  the same `id`, `ok`, and either `result` or `error`.
- `F`: server-to-client samples, with `u32 session | u32 reserved | TTS1 bytes`.
- The server first sends a JSON `ready` message. Session events include a session
  identifier; `app_event` messages report recording and TCP stream state.

Read stdout as binary frames and reserve stderr for diagnostics. Keep reading
frames while waiting for responses; samples and asynchronous events can arrive
between them. This protocol evolves with the extension: use matching versions.
It is separate from the firmware byte protocol and the user-facing TCP JSON stream.

The complete framing contract is in
[`protocol.rs`](https://github.com/hxyulin/tuning-tools/blob/main/crates/studio-server/src/protocol.rs);
the TypeScript client is
[`server.ts`](https://github.com/hxyulin/tuning-tools/blob/main/vscode/src/server.ts).
For scripting live data, the
[TCP stream](https://github.com/hxyulin/tuning-tools/blob/main/docs/stream.md)
is usually the simpler interface.

## Development

```sh
cargo test -p tuning-studio-server
```

Integration tests spawn the real server with a mock target and exercise framing,
connections, inspection, recording and streaming through stdin/stdout.

Part of [Tuning Studio](https://github.com/hxyulin/tuning-tools). MIT licensed.
