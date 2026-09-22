# Packaging and releases

The public package names are `tuning-studio` (desktop), `tuning-studio-api`
(allocation-free firmware API), and `tuning-studio-trace` (optional task event
recorder). Supporting host crates use the `tuning-studio-` prefix as well.
Names were checked on crates.io on 2026-09-22; availability is not a reservation.
The initial release is being prepared. Track remaining work in the
[0.1.0 checklist](release-checklist.md) and review the
[release notes](release-notes/0.1.0.md).

## Firmware integration

`tuning-studio-api` owns the descriptors, framed codec, firmware server and saved
value format. It has no robot, board, executor, allocation or transport dependency.
The host's frame encoder/decoder uses that same crate. The original `rm-telemetry`
v1 table and protocol remain compatible; its magic/version values are deliberately
unchanged. Firmware selects the USB/UART/RTT implementation and decides when to
apply tuning requests or save values.

`tuning-studio-trace` owns the single event-ring implementation. Defaults use
1024 events in `.data.studio_task_trace`. `large-buffer` selects 2048 events;
`custom-section` selects `.task_trace` for board-defined uncached placement.
It requires a critical-section implementation and a suitable monotonic counter.
See each crate's README for integration details.

## Source and binary installation

After publication:

```sh
cargo install tuning-studio --locked
cargo binstall tuning-studio
```

The desktop crate includes prebuilt frontend assets in `frontend/`. End users do
not need Node.js. Developers and release jobs run `npm ci && npm run build` first;
that copies the frontend into the package. These generated files remain ignored
by Git, but the package's explicit include list includes them in the crate.

Linux source builds require WebKitGTK 4.1, GTK 3, libudev and their development
packages; binary installs still need their runtime libraries. Windows needs
WebView2, and probe access may require platform-specific drivers/permissions.
The binstall archive contains the executable and license; native installers are
provided separately. macOS signing/notarization and Windows signing are not yet
configured, so these are unsigned release builds.

## Manual workflow

Run **Actions → Manual release → Run workflow** on `main`. The version input must
match every workspace package and `src-tauri/tauri.conf.json`; it does not edit
versions. Dispatch with both checkboxes off first to verify builds and download
artifacts without publishing anything.

The workflow builds:

| Platform | Target | Native artifact |
|---|---|---|
| Linux x86-64 | `x86_64-unknown-linux-gnu` | deb, AppImage |
| Windows x86-64 | `x86_64-pc-windows-msvc` | NSIS installer |
| macOS Apple Silicon | `aarch64-apple-darwin` | app in DMG |
| macOS Intel | `x86_64-apple-darwin` | app in DMG |

Every target also produces
`tuning-studio-VERSION-TARGET.tar.gz` and a SHA-256 checksum. The archive contains the executable, README and license. Its layout
matches `[package.metadata.binstall]`; downloads come from the `vVERSION` GitHub
release. Draft release assets are not publicly accessible to binstall. Publish
the draft after reviewing its artifacts, release notes and signing limitations.

**Create release** creates a draft only after every build passes, using
`docs/release-notes/VERSION.md` from the candidate commit. It refuses an
existing tag/release instead of overwriting previously released binaries.
**Publish crates** requires the repository secret `CARGO_REGISTRY_TOKEN` with
publishing rights for these packages. It publishes in dependency order after
all builds pass. Publication is permanent; leave this input off for rehearsals.
The secret is exposed only to the publish job. Do not put it in source files.
A partially completed publish must be assessed before rerunning: already-published
versions cannot be overwritten. Bump versions for changed contents.

The initial full workspace cannot be packaged against crates.io until its new
internal dependencies have been published. The build job verifies the two
standalone firmware crates and checks the desktop asset package list. The publish
job lets Cargo verify each dependent package as its prerequisites become available.

Local checks:

```sh
npm ci && npm run build
python3 scripts/release.py validate --version 0.1.0
cargo test --workspace --locked
cargo package -p tuning-studio-api
cargo package -p tuning-studio-trace
```

## Embedded development branch

The RM Embedded integration lives on `feat/tuning`, based on `a6be16e`, with the
original instrumentation commit cherry-picked using `-x`. Future embedded changes
for Tuning Studio belong on that branch; do not add them to
`refactor/boundary-cleanup`. The embedded compatibility crate re-exports the public
API and pins this repository by immutable Git revision until crates.io publication.
After publication, switch those dependencies to the tested registry versions.

## Preparing a version

1. Set the same version in all eight Cargo packages and `tauri.conf.json`, and
   refresh Cargo.lock. Keep frontend/extension package versions aligned.
2. Update the changelog, crate READMEs and `docs/release-notes/VERSION.md`.
   Generated assets must come from that candidate's frontend source.
3. Commit the candidate and run with both publishing inputs disabled.
   Review the four build results and smoke-test the downloaded artifacts.
4. Run with **Create release** enabled to prepare the draft, or enable it on the
   final candidate build. Review its versioned notes, checksums and target files.
5. Configure the registry token and publish only when ready. A GitHub draft
   alone does not publish crates or make binstall downloads available.

Cargo's [publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
explains package verification and immutable registry releases.
