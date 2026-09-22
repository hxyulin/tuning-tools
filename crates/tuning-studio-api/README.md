# tuning-studio-api

General-purpose, allocation-free firmware API for Tuning Studio. No board, RTOS, or transport dependencies.

Descriptors for the values a host tool may watch and tune, over SWD or a
framed byte link.

A firmware declares one static per value and lists them in one `Table`. Over
SWD the host finds the table through the ELF's debug info, samples the cells,
and writes tunable requests into them; the firmware needs no task for that.
Over a byte link such as USB CDC or an RTT channel pair, `server::Server`
answers the same questions in frames defined by `wire`; over USB the host needs
no probe and no ELF. No
allocator either way.

| item | role |
|---|---|
| `Tunable` | an `f32` the host may request; the firmware applies it with `apply` |
| `WatchF32`, `WatchI32`, `WatchU32`, `WatchBool` | values the firmware publishes each tick |
| `Entry` | one descriptor: name, unit, kind, access, range, step and the two cells |
| `Table` | the list the host looks for, with a magic and a format version |
| `wire` | the frame codec: header, CRC-16/MCRF4XX, `Decoder`, `Writer`, `Reader`, `Status` |
| `server::Server` | the firmware end of a framed link: lease, catalog, read, write, discard, save, watch, stats |
| `store` | the saved-values record: encode, decode and restore, and which of two slots to load and overwrite |

```rust
use tuning_studio_api::{Tunable, WatchF32, Table};

pub static PITCH_KP: Tunable = Tunable::new("gimbal.pitch.angle.kp", "1/s", 40.0, 0.0, 200.0, 0.2);
pub static PITCH_ANGLE: WatchF32 = WatchF32::new("gimbal.pitch.angle_rad", "rad");
pub static TABLE: Table = Table::new(&[PITCH_KP.entry(), PITCH_ANGLE.entry()]);

// In your control tick:
let previous_kp = 40.0;
let kp = PITCH_KP.apply(previous_kp);
PITCH_ANGLE.publish(0.5);
```

## The rules

- **The owner decides what runs.** A host write only sets the requested cell.
  `Tunable::apply` replaces a non-finite request with the current value, clamps
  it to `min..=max` (writing the clamped value back), moves at most `max_step`
  per call, and stores the result in the applied cell. A raw probe write cannot
  do more than the declaration allows.
- **Names are ids.** An entry's id is the FNV-1a hash of its name, so a host
  keeps watches and saved values across rebuilds. `Table::validate` rejects two
  names with the same id; call it at boot, which also keeps the table linked.
- **The layout is decoded by field name.** The host reads `Table` and `Entry`
  through DWARF, so field order does not matter. Renaming, retyping or
  redefining a field is a format change and bumps `TABLE_VERSION`.
- **Access is checked on the MCU.** `ReadOnly` refuses every write. A
  `Tunable::safe_only` value is written only while the caller's
  `Context::safe` holds, which the robot derives from its own state (for
  balance-infantry, disarmed). A framed write also needs the lease, a matching
  type tag and a finite value in range.
- **One host writes at a time.** LEASE takes a token for `LEASE_MS`; the host
  renews it. Another token is refused with `Busy` until the lease lapses or is
  released. A lapsed lease leaves the requests where they are; DISCARD asks for
  every default.
- **Samples have a budget.** A WATCH over `SAMPLE_BUDGET_BYTES_PER_S` is
  refused with `Budget` rather than silently thinned; samples the transport
  could not send are counted in STATS.
- **The wire format is this crate.** The host tool uses this same codec; changing a frame bumps `wire::VERSION`.
- **Saves never overwrite the newest record.** A record holds each tunable's
  request by id and kind, with a generation and a CRC. SAVE writes the other
  slot with the next generation, so a torn write leaves the previous save.
  Restore skips ids the table no longer has, changed kinds and values outside
  the current range. The server only sequences SAVE (`save_pending`,
  `finish_save`); the firmware moves the bytes and verifies them.
- **Steps are per call.** `max_step` is sized for the rate the owner calls
  `apply` at; a table's ranges and steps are part of its robot's tuning.

## Testing

```sh
cargo test -p tuning-studio-api
```

The v1 wire format and DWARF descriptor layout remain compatible with the original
`rm-telemetry` API. This crate requires native 32-bit atomics on the firmware target.
Task tracing is an independent optional dependency, `tuning-studio-trace`.

Extracted from rm-embedded-rs `rm-telemetry`; original MIT/Apache-2.0 notices
are included in this package.
