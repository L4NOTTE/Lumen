# Lumen Linux Chromium compatibility gate

Isolated Rust/CEF experiment, **not a browser release or a completed Linux port**.
The production Windows and WebKitGTK executables are unchanged by this crate.

## GitHub check

Add this `linux-chromium/` folder and
`.github/workflows/linux-chromium-probe.yml` to a separate branch in the source
repository, then open a draft pull request to main. The compatibility workflow
runs on the PR. If the workflow is added to the default branch, it can also be
started manually in Actions as `linux-chromium-compatibility`.

The current GitHub integration could read L4NOTTE/Lumen but returned HTTP 403
(`Resource not accessible by integration`) on branch creation. No remote branch
or PR was created and no workflow run is claimed here.

The first gate uses Chrome-style CEF 156.4.0 and a disposable Manifest V3 fixture.
It must demonstrate extension loading, a running service worker, storage, tabs,
and the presence of tabCapture/offscreen APIs. The fixture reports its deterministic
extension ID through the title of a dedicated loopback page. Rust accepts reports
only for that page and ID. A missing extension times out; a failed capability check
exits unsuccessfully. Merely creating a Chromium window is not a passing gate.

Sandbox is enabled. CDP is disabled. The profile is a TempDir, never a user's Lumen
profile. The probe rejects sandbox-disable and remote-debugging command-line flags.
Unpacked extension loading is only an experiment in this disposable profile.

## Build and run on Ubuntu 24.04 / Mint 22

Install the development/runtime packages listed in
`.github/workflows/linux-chromium-probe.yml`, then:

```bash
cargo build --manifest-path linux-chromium/Cargo.toml --release --locked
cd linux-chromium/target/release
export LD_LIBRARY_PATH="$PWD${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
timeout 60s ./lumen-chromium-probe
```

CEF's build script obtains the pinned native distribution and copies runtime
files next to the executable. Download/build can be large. Do not pass
`--no-sandbox` if your environment refuses user namespaces: diagnose the sandbox
configuration instead. CI permits namespaces only for this probe's AppArmor profile.

The GitHub workflow runs a real native build under Xvfb and uploads diagnostics
even on failure. A red gate means this backend has **not** proved compatibility;
do not silently skip the extension test or publish a finished browser artifact.

## Local validation so far

`cargo check --target x86_64-unknown-linux-gnu --features cef/dox --locked`
passes on the Windows development machine. `dox` intentionally omits native CEF
linking; it is used only for Rust API/type checking. It must **never** be enabled
in the native CI build. Fixture scripts pass Node syntax checks.

## Gates still required

1. Native extension capability gate above on Ubuntu.
2. Real Shazam/VK Next installation, popup origin, background/service-worker
   behavior and permission flows. Third-party packages are not redistributed here.
3. Actual audio capture of the active browser tab. `fixture/popup.html` provides
   a manual stream-ID check when opened from the extension action; API presence
   or a stream ID alone does not prove that samples/audio recognition work.
4. Reuse Lumen's UI and port tabs, downloads, filters, zoom, sleep, transfer,
   Inspector and user settings only after engine viability is established.
5. X11/Wayland, media, sandbox and security regressions before releasing Linux.

References:
- https://github.com/tauri-apps/cef-rs
- https://github.com/chromiumembedded/cef
- https://docs.rs/cef/156.4.0+156.0.26/cef/
- https://developer.chrome.com/docs/extensions/reference/api/tabCapture/

Current tauri-runtime-cef does not implement browser_extensions_enabled or
extensions_path. This prototype deliberately tests native CEF directly instead
of assuming those Tauri attributes work.
