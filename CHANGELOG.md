# Changelog

## 0.1.0 — unreleased

Initial release candidate. See the [release notes](docs/release-notes/0.1.0.md)
for installation, packages and known limits.

### Added

- Bounded tuning sliders with coalesced writes and precise numeric input.
- Catalog-aware Live Watch entries with typed applied values, units, tuning
  controls, applied-value plotting and an explicit raw-descriptor view.
- `Server::without_storage` and `Status::SaveUnsupported` for volatile firmware;
  Studio disables Save after this explicit response. Legacy storage errors now
  describe unavailable storage or write failure without assuming a flash fault.


- Desktop and VS Code interfaces backed by a shared Rust acquisition engine.
- Live variable trees, pointer following, enum payloads, paged arrays/slices and
  persistent pinned watch groups.
- Numeric plots, MCAP recording, CSV export, live TCP streaming and RTT defmt logs.
- Named tuning controls over SWD and the firmware byte protocol, including
  lease-controlled writes and firmware-managed persistence.
- Embassy task-state inspection and optional instrumented execution timelines.
- Public `no_std` firmware API and trace crates, shared host protocol code, and
  versioned packages for the desktop, server and host libraries.
- Manually triggered native release builds, binstall archives and crate publication.

### Fixed

- Make TCP client I/O cancellable so a stalled peer cannot hold up stream
  cleanup while its socket worker is being joined.

### Compatibility

- Preserve `rm-telemetry` v1 descriptor/protocol compatibility and the v1 trace format.
- Keep the VS Code backend executable named `studio-server`.
