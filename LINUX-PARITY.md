# Linux feature parity — work in progress

Состояние на 18 ноября 2026 года — работа над Linux-портом приостановлена. Текущие изменения 0.5.0 проверяются на Windows. Ниже сохранены сведения о ранее подготовленной Linux-ветке.

## Targets for the Windows 0.4.77 port

Ubuntu 24.04, Fedora 44 and Arch Linux on x86_64 GNU are the initial build targets. The preview now reuses the Windows local-history search suggestions and clears them when browsing history is deleted. The Fedora and Arch jobs build and link against each distribution's real system libraries. Ubuntu also has an Xvfb startup check. Desktop checks with real window managers on X11 and Wayland are still required.

Local native validation on Ubuntu 24.04.5, Fedora 44 and Arch Linux completed with 21 passing Rust tests per distribution, a linked debug executable and `LUMEN_LINUX_SMOKE_OK` under Xvfb. The earlier public Ubuntu CI job failed at Cargo dependency resolution because its source snapshot lacked the vendored `glib-macros` package. The local validation archive included the vendor tree.

The sandboxed CEF probe on Ubuntu passed the Manifest V3 fixture. It ran a service worker, wrote and read extension storage, queried tabs and detected the tabCapture and offscreen APIs. Popup identity, actual audio capture, installed extensions, profile permissions and incognito behavior remain to be tested.

Session restore creates the WebView for the active tab at startup. Other entries keep their URLs and titles and get a WebView on first selection. Automatic unloading of already opened tabs is still pending.

Remaining Windows functionality to validate or port: the Xray/VLESS VPN and failure isolation, network and cosmetic ad filtering, favicon persistence, floating favorites, tab detach and transfer, background tab memory management, internal download notifications, media permissions, and WebView process failure recovery equivalents. Windows DPAPI protection must be replaced with a Linux credential-store design for VPN secrets. Test packages and update behavior on each distribution before declaring full parity.

This is not a release declaration. The published 0.4.6 archives remain unchanged.

Current source additions:
- Per-user default browser registration through XDG, with a generated desktop
  entry, correct Exec quoting, no shell interpolation and verification of the
  selected handler. The UI asks before changing defaults. Work is off the UI thread.
- Native WebKit navigation policy distinguishes middle clicks from foreground
  new-window links. Background tabs preserve the active tab; navigation admission
  still passes through the existing scheme/internal-document/blocker checks.

Validation: desktop-entry generator tested on Windows; Windows cargo check and
shared JS contract pass. Linux source parses with rustfmt. XDG behavior, WebKit signals and real input must also be checked in full desktop sessions.

Unresolved engine decision:
- Wry uses WebKitGTK on Linux, not WebView2.
- Replacing it with CEF does not automatically provide Chrome extensions:
  the current tauri-runtime-cef implementation explicitly does not support its
  browser_extensions_enabled / extensions_path attributes.
- A Chromium-based alternative must first prove extension installation, service
  workers, popup identity, activeTab and audio capture with Shazam/VK Next on
  Ubuntu. Do not replace the production engine or promise parity before that.
- Any prototype must retain sandboxing and keep unauthenticated CDP disabled by
  default. Do not solve compatibility by globally disabling the sandbox.

Still required: installed-extension testing, package installation and updates, favicon
adapter, background/sleeping tabs, inter-window transfer, equivalent ad filtering,
DevTools presentation, media permissions and X11/Wayland tests.

References:
- https://github.com/tauri-apps/wry
- https://docs.rs/tauri-runtime-cef/latest/src/tauri_runtime_cef/webview.rs.html
- https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
