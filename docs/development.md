# Development and verification

Run commands from the repository root. Build frontend assets with `npm ci &&
npm run build` before compiling the desktop host directly with Cargo.

## Local checks

```sh
npm test  # Node 24: tuning presentation and coalesced slider writes
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets -- -D warnings
npm --prefix vscode ci
npm --prefix vscode run typecheck
cargo build -p tuning-studio-server
npm run build:webview
npm --prefix vscode run harness
npm --prefix vscode run test:recovery
npm --prefix vscode run test:trace
npm --prefix vscode run test:inspector
```

The mock inspector uses the committed ELF fixture and does not require a probe.
See the [inspector lab](../crates/studio-dwarf/tests/fixtures/README.md) for building
and flashing the STM32H723 fixture and running its hardware checks. Do not run a
lab hardware check against unrelated firmware.

The browser mock includes tuning descriptors under the `tuning` namespace.
Run `npm run dev` and open `http://localhost:1420/?host=mock&save=unsupported`
to exercise volatile-save feedback, or `&save=storage` for a legacy failure.
These flags affect only the browser mock. The firmware constructor and
status code were added in 0.1.1; an existing 0.1.0 image still reports its old status
until rebuilt with the new API.

## Real firmware and probes

```sh
STUDIO_DWARF_ELF=path/to/firmware cargo test -p tuning-studio-dwarf -- --ignored
cargo run -p tuning-studio-core --example watch -- --elf path/to/firmware --list
cargo run -p tuning-studio-core --example watch -- --elf path/to/firmware --chip STM32H723VG static_path
cargo run --release -p tuning-studio-carriers --example probe_bench -- STM32H723VG
```

The parser check reads only the ELF. The watch example attaches to the target;
use the ELF that matches the flashed firmware. The probe benchmark measures
hardware read latency and needs exclusive access to the probe.

For a firmware that implements the byte-link protocol:

```sh
cargo run -p tuning-studio-core --example link -- --port /dev/cu.usbmodem101 --rate 500 value_name
```

Replace chip names, paths, ports and value names with those for your target.
See the example source for optional tuning/save arguments; these write to the
firmware and are unnecessary for a read-only sample check.

## Firmware crates and packaging

```sh
cargo test -p tuning-studio-api
cargo test -p tuning-studio-trace --all-features
cargo check -p tuning-studio-api --target thumbv7em-none-eabihf
cargo check -p tuning-studio-trace --all-features --target thumbv7em-none-eabihf
python3 scripts/release.py validate --version 0.1.1
```

Install the ARM target with `rustup target add thumbv7em-none-eabihf` first.
Packaging and the manual release workflow are described in [releases](releases.md).
