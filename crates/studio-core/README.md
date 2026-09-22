# tuning-studio-core

The acquisition engine for Tuning Studio: coalesced memory reads, timed sampling,
session control, tuning, RTT/defmt logs, and binary sample frames. Use it to build
a custom host tool without the desktop or editor UI.

For an application-level backend with recording and Live Watch, start with
`tuning-studio-app`. Firmware should use `tuning-studio-api`, not this crate.

## Use it

For a published release:

```toml
[dependencies]
tuning-studio-core = "0.1.0"
tuning-studio-carriers = "0.1.0"
tuning-studio-dwarf = "0.1.0"
```

The Rust library name is **`studio_core`**. This example samples a mock value:

```rust
use studio_carriers::{mock::MockLink, Link};
use studio_core::{ReadItem, ReadPlan};
use studio_dwarf::VariableType;

fn main() -> studio_carriers::Result<()> {
    let mut target = MockLink::new();
    target.poke(0x2000_0000, &12.5f32.to_le_bytes());
    let mut plan = ReadPlan::new(vec![ReadItem {
        id: 1,
        address: 0x2000_0000,
        scalar: VariableType::F32,
        bit_offset: None,
        bit_size: None,
    }]);
    let mut values = Vec::new();
    target.with_memory(&mut |memory| {
        let outcome = plan.sample(memory, &mut values);
        assert_eq!(outcome.regions_failed, 0);
    })?;
    assert_eq!(values, vec![12.5]);
    Ok(())
}
```

## Building blocks

- `ReadPlan` sorts and merges nearby reads while preserving caller order.
  Defaults allow a 64-byte gap and bound merged regions to 512 bytes.
- `Session`, `SessionCommand` and `SessionSink` own acquisition on a worker
  thread and report frames, connection state, diagnostics and log events.
- `catalog` and `tune` discover firmware descriptors and validate tuning requests.
  The `wire` module uses the same codec as `tuning-studio-api`.
- `frame` encodes columnar TTS1 samples; `Tap` distributes samples and events to
  independent consumers ahead of the UI sink.
- `schedule`, `stats`, `log` and `rtt_tuning` handle deadlines, measured transport
  costs, defmt decoding and the firmware control channel.

Coalescing reduces memory transactions; it is not the raw CMSIS-DAP multi-command
packet optimization. Reads happen while firmware runs and do not form an atomic
snapshot. Read failures are reported and missing numeric samples use NaN.
Rates depend on transport latency and host scheduling; numeric samples use `f64`
and cannot exactly represent every 64-bit integer.

## Development

From the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo test -p tuning-studio-core
cargo run -p tuning-studio-core --example watch -- --elf firmware.elf --list
```

The [development guide](https://github.com/hxyulin/tuning-tools/blob/main/docs/development.md)
includes hardware and serial examples. The
[TCP protocol](https://github.com/hxyulin/tuning-tools/blob/main/docs/stream.md)
is served by the app crate; TTS1 itself is documented in this crate's `frame` module.

Part of [Tuning Studio](https://github.com/hxyulin/tuning-tools). Host-only (`std`),
MIT licensed. Use matching 0.1 workspace versions.
