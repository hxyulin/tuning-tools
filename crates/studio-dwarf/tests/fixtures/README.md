# Test Fixtures

This directory contains test ELF binaries for testing the DWARF parser and ELF loading functionality.

## Fixtures

### test_arm.elf
A minimal ARM Cortex-M4 binary containing:
- `global_counter`: volatile uint32_t at fixed address
- `sensor_data`: volatile float at fixed address
- Simple main loop

**Source:** `test_source.c`

### test_struct.elf
An ARM binary with struct types:
- `SensorData` struct with x, y, value fields
- `sensor_struct`: volatile SensorData instance

**Source:** `test_struct_source.c`

### test_pointer.elf
An ARM binary with pointer types:
- `data_ptr`: uint32_t* pointer
- Pointer dereferencing scenarios

**Source:** `test_pointer_source.c`

## Building Fixtures

To rebuild the fixtures, you need the ARM GCC toolchain:

```bash
# Install ARM GCC (Ubuntu/Debian)
sudo apt-get install gcc-arm-none-eabi

# Install ARM GCC (macOS)
brew install --cask gcc-arm-embedded

# Build
cd tests/fixtures
make all
```

## Linker Script

The fixtures use a simple linker script (`link.ld`) that places:
- `.text` at 0x08000000 (Flash)
- `.data` at 0x20000000 (RAM)
- `.bss` at 0x20001000 (RAM)

This matches typical STM32 memory layouts for testing.

## Rust fixtures

### rust_v0.elf, rust_legacy.elf
The same firmware-shaped program (`rust_embedded/`) built for
`thumbv7em-none-eabihf` with the rm-embedded-rs release profile (thin LTO,
`opt-level = "s"`, 8 codegen units, full debug info). Statics are mangled and
live in nested modules: atomics, a `[u16; 8]`, a `u64`, and a
`Shared<Gimbal<4>>` holding a fieldless enum, `Option<f32>`, a data-carrying
enum, a niche-encoded `Option<NonZeroU32>`, an array and a tuple.

`rust_v0.elf` uses the default v0 mangling (`_R...`); `rust_legacy.elf` uses
legacy mangling (`_ZN...17h<hash>E`), which stable rustc only allows behind
`-Z unstable-options`.

```bash
cd tests/fixtures/rust_embedded
cargo build --release
cp target/thumbv7em-none-eabihf/release/rust_fixture ../rust_v0.elf
RUSTC_BOOTSTRAP=1 RUSTFLAGS="-C link-arg=-Tlink.x -Z unstable-options -C symbol-mangling-version=legacy" \
  cargo build --release --target-dir target-legacy
cp target-legacy/thumbv7em-none-eabihf/release/rust_fixture ../rust_legacy.elf
```

### embassy_tasks.elf
A small embassy program (`embassy_tasks/`) on embassy-executor 0.10, the
version rm-embedded-rs uses, for the task view: `main`, `blink::blink_task`
(an argument, a local held across two `.await`s) and `worker` with
`pool_size = 2`. Built with full LTO and one codegen unit to keep the file
small; dependencies carry no debug info, as the task types are described by
the fixture's own compile unit. `tasks.rs` tests check the `.await` line
numbers in `src/main.rs`, so update them when the source moves.
`src/rm_task_stats.rs` is a copy of rm-embedded-rs' `rm-task-stats` crate
(`src/lib.rs` without its tests and `#![no_std]`), with embassy-executor's
`trace` feature on, for `task_stats.rs`; refresh it when that crate changes its
layout.

```bash
cd tests/fixtures/embassy_tasks
cargo build --release
cp target/thumbv7em-none-eabihf/release/embassy_fixture ../embassy_tasks.elf
```

### ../../../studio-core/tests/fixtures/rm_telemetry.elf
The `telemetry` binary of the same project: an `rm_telemetry::Table` with two
tunables and four watches of every cell kind, for the catalog decoder in
`studio-core`. `src/bin/telemetry/rm_telemetry.rs` is a copy of the
`rm-embedded-rs` crate's `src/lib.rs` without its tests and crate attributes;
refresh it when the firmware crate changes its layout.

```bash
cd tests/fixtures/rust_embedded
cargo build --release --bin telemetry
cp target/thumbv7em-none-eabihf/release/telemetry ../../../../studio-core/tests/fixtures/rm_telemetry.elf
```

Built with rustc 1.98.1. Type names such as `Atomic<u32>` follow the core
library of that toolchain; rebuilding with another toolchain may change them.

### inspector_lab.elf

Runnable STM32H723 SWD inspection firmware, built from
`embassy_tasks/src/bin/inspector_lab.rs`. It uses the reset 64 MHz HSI clock,
SysTick, and DTCM, with caches left disabled. It does not initialize GPIO, CAN,
USB, motors, or board peripherals. Flashing replaces the board's existing image.

It contains nested structs/tuples/arrays, a 300-element array, fieldless and
payload enums, `Option<f32>`, niche-encoded `Option<NonZeroU32>`, `Result`, a
union, all common scalar widths, Unicode, NaN/Infinity/signed zero, null and
changing pointers, a pointer-to-pointer, a pointer-to-array, and a cyclic list.
Embassy tasks include a two-await producer, a consumer with retained struct
locals, a permanently waiting task, an unused pool slot, a completed task, and
`rm-task-stats` counters. Only the producer uses the single-waiter SysTick future.

Build from this directory:

```sh
(cd embassy_tasks && cargo build --release --bin inspector_lab --features trace)
cp embassy_tasks/target/thumbv7em-none-eabihf/release/inspector_lab inspector_lab.elf
```

From the repository root, test the ELF's initialized data without hardware:

```sh
cargo build -p studio-server
npm --prefix vscode run test:inspector
```

To test a connected STM32H723VGTx, explicitly flash and reset it, then run the
same read-only suite against hardware (add `--probe <selector>` to probe-rs when
more than one probe is attached; the test script uses the first enumerated probe):

```sh
probe-rs download --chip STM32H723VGTx crates/studio-dwarf/tests/fixtures/inspector_lab.elf
probe-rs reset --chip STM32H723VGTx
npm --prefix vscode run test:inspector -- --hardware
```

The hardware suite observes several seconds of changing pointer values and task
states; it does not write memory, halt, reset or flash the target. The producer
initializes the large array to its indices on its first update (within one second
of reset); the hardware suite waits up to five seconds for that update.
The lab is left running afterward. Open `inspector_lab.elf` in the desktop app
or VS Code to explore it interactively. Character readouts include the character and code point. Live Watch selects
active enum payloads automatically. The lab also exposes borrowed UTF-8 strings,
slices and the `STUDIO_TASK_TRACE` event buffer for the execution timeline.
