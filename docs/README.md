# Documentation

- `references/` — architecture notes on datavis-rs, herkules-tools, MemRW3 and RM Studio, written before any code so their designs could be reused or avoided deliberately.
- `adr/` — decision records, one file per irreversible choice.
- `stream.md`: the live TCP data stream protocol, for your own scripts (example client in `examples/stream_client.py`).

- [Task timeline](task-timeline.md) — investigate Embassy polls, navigate captures, and understand timing and data-loss limits.
- [Live Watch](../README.md#live-watch-swd) — inspect values, follow pointers, page arrays and save watch groups.
- [Firmware trace integration](../crates/studio-task-trace/README.md) — add the RAM event ring and Embassy hooks.
- [Inspector lab](../crates/studio-dwarf/tests/fixtures/README.md#inspector_labelf) — build and test complex types and traced tasks on STM32H723.
