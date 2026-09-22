# Tuning Studio

Desktop live variable inspection, tuning, telemetry and task timelines for
embedded firmware. Install with `cargo install tuning-studio --locked` or
`cargo binstall tuning-studio` once a release is published.

Source installation embeds prebuilt frontend assets; Node.js is not needed by
end users. Linux needs WebKitGTK 4.1, GTK 3 and libudev development libraries to
compile (runtime libraries for binary installs). Windows requires WebView2.
macOS requires the system WebKit framework. Native app bundles are also provided
through [GitHub Releases](https://github.com/hxyulin/tuning-tools/releases).

See the repository documentation for debug probe access and firmware integration.
