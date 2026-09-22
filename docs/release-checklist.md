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
- [ ] All four release build jobs pass for the candidate commit.
- [ ] Download and verify archive checksums and executable names.
- [ ] Smoke-test native startup on each release platform; a successful build alone
      does not verify webview rendering or device access.
- [ ] Review the draft notes and accept the documented unsigned-installer limitations.
- [ ] Create the GitHub draft from verified artifacts and check all four target archives.

## Publication

- [ ] Configure the `CARGO_REGISTRY_TOKEN` repository secret with appropriate
      publishing rights. This secret was absent during preparation.
- [ ] Recheck package-name availability immediately before first publication.
- [ ] Publish crates in dependency order with the manual workflow. The initial
      registry build of dependent crates is verified as prerequisites become available.
- [ ] Publish the reviewed GitHub draft so binstall can access its assets.
- [ ] Test `cargo install tuning-studio --version 0.1.0 --locked` and
      `cargo binstall tuning-studio --version 0.1.0` from a clean environment.
- [ ] Test the firmware crates from crates.io, then update RM Embedded's
      `feat/tuning` dependencies from immutable Git pins to verified registry versions.
- [ ] Date the changelog and replace preparation notices in the READMEs after
      confirming that installation works.

Do not reuse version 0.1.0 for changed contents after a crate is published.
If a publish job stops partway through, inspect which packages succeeded before
retrying; immutable published versions cannot be overwritten.
