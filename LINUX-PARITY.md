# Linux feature parity — work in progress

This is not a release declaration. The published 0.4.6 archives remain unchanged.

Current source additions:
- Per-user default browser registration through XDG, with a generated desktop
  entry, correct Exec quoting, no shell interpolation and verification of the
  selected handler. The UI asks before changing defaults. Work is off the UI thread.
- Native WebKit navigation policy distinguishes middle clicks from foreground
  new-window links. Background tabs preserve the active tab; navigation admission
  still passes through the existing scheme/internal-document/blocker checks.

Validation: desktop-entry generator tested on Windows; Windows cargo check and
shared JS contract pass. Linux source parses with rustfmt. These are not native
Linux compilation or runtime tests. XDG behavior, WebKit signals and real input
must be checked in Ubuntu/Mint before shipping.

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

Still required: native Linux build/run, extension compatibility prototype, favicon
adapter, background/sleeping tabs, inter-window transfer, equivalent ad filtering,
DevTools presentation, media permissions and X11/Wayland tests.

References:
- https://github.com/tauri-apps/wry
- https://docs.rs/tauri-runtime-cef/latest/src/tauri_runtime_cef/webview.rs.html
- https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html
