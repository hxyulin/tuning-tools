# Initial release checklist — 0.1.0

This is a preparation record, not a claim that publication has completed.
Use [the release workflow](releases.md) and the committed
[release notes](release-notes/0.1.0.md). Update verification results for the exact
candidate commit before publication.

## Prepared

- [x] Repository transferred to `hxyulin/tuning-tools`; package links point there.
- [x] All eight package names checked on crates.io (2026-09-22; not reserved).
- [x] Package versions set to 0.1.0 with descriptions, licenses and READMEs.
- [x] Desktop package includes generated frontend assets for Node-free source installs.
- [x] Firmware API extracted and used by the host codec and RM Embedded compatibility facade.
- [x] RM Embedded integration isolated on `feat/tuning` with immutable Git dependencies.
- [x] Manual workflow builds four desktop targets and produces binstall archives/checksums.
- [x] Main/crate READMEs, user/development guides and initial release notes prepared.

## Candidate verification

- [x] Local formatting, workspace tests, Clippy and all five runnable README examples pass.
      All eight package lists include README/license files; the four independent
      crates pass `cargo package` verification. Dependent registry verification
      awaits publication of their prerequisites.
- [x] All four release build jobs passed for `dbcb728` in [run 35682106024](https://github.com/hxyulin/tuning-tools/actions/runs/35682106024).
- [x] Downloaded all four binstall archives and verified SHA-256 checksums, executable names and contents.
- [x] Apple Silicon DMG and Intel DMG under Rosetta opened and rendered the UI on macOS.
- [ ] Native Windows/Linux and physical Intel Mac runtime checks remain outstanding;
      CI verifies their builds and tests. These startup checks do not verify device access.
- [x] Reviewed draft notes and documented unsigned-installer limitations.
- [x] Created the GitHub draft from verified artifacts: five native installers and four archives with checksums.

## Publication

- [x] Configured the `CARGO_REGISTRY_TOKEN` repository secret for publication.
- [x] Rechecked all eight package names immediately before first publication.
- [x] Published all eight crates in dependency order through CI and the local
      recovery script after the new-crate rate limit.
- [x] Published the reviewed GitHub release and verified Apple Silicon binstall installation.
- [ ] Test `cargo install tuning-studio --version 0.1.0 --locked` and
      `cargo binstall tuning-studio --version 0.1.0` from a clean environment.
- [x] Verified registry firmware dependencies with host tests and normal/timeline
      firmware builds; migration is in RM Embedded PR #41.
- [ ] Date the changelog and replace preparation notices in the READMEs after
      confirming that installation works.

Do not reuse version 0.1.0 for changed contents after a crate is published.
If a publish job stops partway through, inspect which packages succeeded before
retrying; immutable published versions cannot be overwritten.
