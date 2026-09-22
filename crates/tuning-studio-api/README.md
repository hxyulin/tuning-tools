# tuning-studio-api

An allocation-free, `no_std` firmware API for named tuning parameters and
telemetry. Expose a table to Tuning Studio over SWD, or serve the same values
over a byte transport such as USB CDC, UART or RTT. The crate has no board,
RTOS, allocator or transport dependency.

You do not need this crate to inspect ordinary variables over SWD. Add it when
you want discoverable names, units, ranges, controlled parameter application,
or an ELF-free serial connection.

## Add it to firmware

For a published release:

```toml
[dependencies]
tuning-studio-api = "0.1.1"
```

For unreleased development, use a path to `crates/tuning-studio-api` in a local checkout,
or pin the repository to an immutable Git revision. The target must support
native 32-bit atomics. No features or runtime initialization are required for
the descriptors themselves.

```rust
use tuning_studio_api::{Table, Tunable, WatchF32};

static SPEED_KP: Tunable = Tunable::new("speed.kp", "1/s", 4.0, 0.0, 20.0, 0.1);
static SPEED: WatchF32 = WatchF32::new("speed.measured", "rad/s");
static TABLE: Table = Table::new(&[SPEED_KP.entry(), SPEED.entry()]);

fn control_tick(current_kp: f32, measured_speed: f32) -> f32 {
    SPEED.publish(measured_speed);
    SPEED_KP.apply(current_kp)
}

fn main() {
    // Do this at firmware startup; it detects duplicate IDs and retains the table.
    TABLE.validate().expect("unique tuning names");
    // In real firmware, retain the returned gain for the next control iteration.
    let next_kp = control_tick(4.0, 12.5);
    assert_eq!(next_kp, 4.0);
}
```

For SWD, retain DWARF information in the firmware ELF and open the ELF matching
the flashed image. The host locates the table by its fields and reads descriptor
addresses from it. Descriptors must stay linked; call `TABLE.validate()` at boot.
The host writes a requested value; your control loop decides when to apply it.

## Available pieces

| API | Purpose |
|---|---|
| `Tunable` | An `f32` request/applied pair with range and per-call step limit |
| `WatchF32`, `WatchI32`, `WatchU32`, `WatchBool` | Values published by firmware |
| `Entry`, `Table` | Discoverable names, units, kinds, access and cell addresses |
| `wire` | Fixed-buffer framing, CRC, decoder, reader/writer and status codes |
| `server::Server` | Lease, catalog, read/write, defaults, save and sample requests |
| `store` | CRC-protected records, restore and alternating-slot selection |

## USB, UART or RTT integration

The crate owns neither a transport nor a clock. For each link:

1. Construct a `wire::Decoder` and `server::Server::new(&TABLE)`.
2. Feed received bytes into the decoder. Pass each decoded header/payload to
   `Server::handle`, with `Context { now_us, safe, bad_frames }` and an output
   buffer of `wire::MAX_FRAME` bytes. Transmit the returned reply bytes.
3. Call `Server::sample(now_us, ...)` when due (`next_sample_us()` exposes the
   next deadline), and transmit nonempty sample frames.
4. If `save_pending()` is true, serialize the requested values with `store`,
   write and verify the storage slot, then call `finish_save()` with the result.

USB CDC/UART drivers and RTT channel setup remain in firmware. A SAVE response
is deferred until the caller completes storage. Without that integration, the
protocol does not magically persist values. For a deliberately volatile demo,
use `Server::without_storage(&TABLE)` (since 0.1.1): SAVE returns
`Status::SaveUnsupported` (12) immediately and never sets `save_pending()`.
`Server::new` retains its existing deferred-save behavior. Status 11 remains
ambiguous for older firmware: storage may be absent or a write may have failed.
No wire version or existing status code changes. Restore saved values at startup
using `store` before running the control loop.

## Contracts and limits

- `apply` replaces a non-finite request with the current value, clamps it to the
  declared range, and moves by at most `max_step` per call. Choose the step for
  your control-loop rate; it is not a per-second limit.
- Entry IDs are FNV-1a hashes of names. Keep names stable across firmware updates;
  `Table::validate` rejects duplicate IDs, including hash collisions.
- Framed writes check type, range, a renewable lease and access policy.
  `SafeOnly` uses the caller-supplied `Context::safe`. Firmware must derive that
  flag from its own state. These are protocol checks, not protection against
  arbitrary debugger memory writes. `apply` does not itself check `Context`.
- The server supports up to 32 subscribed values and a 64,000-byte/s sample
  budget. Late sample deadlines are counted; the transport must handle its own
  transmit buffering and failures.
- `store` chooses the other slot when saving, validates records with a CRC, and
  skips restored entries with missing IDs, changed kinds or invalid ranges.
  Actual flash erase/write behavior and power-failure handling belong to the
  firmware's storage adapter.
- Host and firmware share this codec. Changing the framed protocol requires a
  `wire::VERSION` change; changing descriptor field contracts requires a
  `TABLE_VERSION` change. The current v1 formats retain `rm-telemetry` compatibility.

Task timing is independent: use the optional `tuning-studio-trace` crate when
an execution timeline is needed. This crate imposes no Embassy dependency.

## Development and license

From the [repository](https://github.com/hxyulin/tuning-tools):

```sh
cargo test -p tuning-studio-api
cargo check -p tuning-studio-api --target thumbv7em-none-eabihf
```

See [Tuning Studio](https://github.com/hxyulin/tuning-tools) for the desktop and
VS Code interfaces. Extracted from `rm-embedded-rs`'s `rm-telemetry` crate;
licensed MIT OR Apache-2.0 with the original notices included.
