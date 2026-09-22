# tuning-studio-carriers

Hardware transports and test doubles for Tuning Studio. This host-side library
provides probe-rs SWD memory access, RTT channels, serial byte streams and a
sparse in-memory target for tests. It contains no UI or sampling scheduler.

## Use it

For a published release:

```toml
[dependencies]
tuning-studio-carriers = "0.1.1"
```

The Rust library name is **`studio_carriers`**. A mock target needs no hardware:

```rust
use studio_carriers::{mock::MockLink, Link};

fn main() -> studio_carriers::Result<()> {
    let mut target = MockLink::new();
    target.poke(0x2000_0000, &42u32.to_le_bytes());
    let mut bytes = [0; 4];
    let mut read_result = Ok(());
    target.with_memory(&mut |memory| {
        read_result = memory.read(0x2000_0000, &mut bytes);
    })?;
    read_result?;
    assert_eq!(u32::from_le_bytes(bytes), 42);
    Ok(())
}
```

## Interfaces

| Interface/module | Responsibility |
|---|---|
| `MemoryAccess` | Read and write addressed target memory without halting for reads |
| `Link` | Hold memory access open across a batch of operations |
| `ByteStream` | Ordered bytes for the firmware protocol |
| `probe` | Discover/open probes, select a target chip and configure SWD speed |
| `rtt` | Discover channels and move bytes through target RAM |
| `serial` | Discover/open serial devices |
| `mock` | Seed memory, inspect reads and inject read faults for tests |
| `cortex_m` | Cortex-M run-state inspection |

A session worker should own its carrier. Keep related memory reads inside one
`with_memory` call to avoid repeated access-port setup. The traits use mutable
access; this layer does not coordinate competing debugger processes.

Hardware access needs the appropriate USB permissions/drivers. Linux builds
need libudev development headers. Another debugger holding the probe must
release it before connection. Reading RTT logs updates target channel offsets,
so RTT consumption involves writes even when sampled variables are read-only.
Use plain memory reads when a strict no-write inspection is required.

## Development

From the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo test -p tuning-studio-carriers
cargo run --release -p tuning-studio-carriers --example probe_bench -- STM32H723VG
```

The benchmark needs a connected target and exclusive probe access. For sampling
and sessions, use `tuning-studio-core`; for a complete application backend, use
`tuning-studio-app`.

Part of [Tuning Studio](https://github.com/hxyulin/tuning-tools). Host-only (`std`),
MIT licensed. The 0.1 Rust API is evolving.
