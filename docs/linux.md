# Experimental Linux build

This fork enables the existing GTK3/WebKitGTK desktop backend. Linux remains
experimental; it does not have macOS/Windows feature parity.

## Requirements

- Rust from `rust-toolchain.toml`.
- Node.js from `.node-version` and pnpm from `package.json`.
- GTK3, WebKitGTK 4.1, libsoup 3, OpenSSL, D-Bus, a C/C++ compiler, and pkg-config.
- A graphical Wayland or X11 session.

On Arch Linux, the native build dependencies can be installed with:

```sh
sudo pacman -S --needed base-devel pkgconf gtk3 webkit2gtk-4.1 libsoup3 openssl dbus
```

The application checks the installed WebKitGTK version before constructing any
webview. The current minimum is 2.54.0; 2.54.1 is the reviewed version. Do not
set environment overrides that disable the WebKit sandbox or its mitigations.

## Build and run

```sh
pnpm install --frozen-lockfile
pnpm dev
```

To produce a standalone executable with embedded frontend assets, without
building distribution packages:

```sh
pnpm -C desktop exec tauri build --no-bundle
./target/release/zephium-desktop
```

For a faster development build, add `--debug`; the executable then lives at
`target/debug/zephium-desktop`. These standalone builds share the normal
`app.zephium` profile. `pnpm dev` uses the separate `app.zephium.dev` profile.

On machines with limited memory, set `CARGO_BUILD_JOBS=4` and
`CARGO_PROFILE_DEV_DEBUG=0` for the debug build.

## Interface flicker

On the tested WebKitGTK 2.54.1 system, animated interface transitions can blink
or briefly show an old selection. Enable **Settings → Appearance → Reduce
Motion** to avoid this. The preference persists across launches. The same
flicker occurs under native Wayland and XWayland; switching display backends
does not resolve it.

## Enabled Linux integrations

Website tabs opt into browser dialogs separately from the privileged app UI.
The app UI still uses its authorized native commands for file access.

- Downloads use the shared profile-scoped coordinator: destination selection,
  progress, cancellation, history, collision-safe publication, and file
  open/reveal. Private-window records remain in memory. Native WebKitGTK
  downloads cannot pause or resume.
- Uploads use a portal-capable GTK file picker, including MIME filters and
  multiple-file selection. Save and directory pickers use the same mechanism.
  `GTK_USE_PORTAL=1` is selected at startup unless explicitly configured.
- Camera, microphone, screen capture, location, notifications and pointer lock
  requests can ask for one-time consent. A new document invalidates an open
  permission or upload decision. Actual device availability also depends on
  the installed WebKitGTK build and desktop services. Clipboard reading and
  unknown permission kinds remain denied.
- Website authentication, JavaScript, color and print dialogs use WebKitGTK's
  native handlers. Website fullscreen and external application links are
  enabled for ordinary tabs. User-clicked GET links targeting a new window
  open as shell-owned tabs; POST popups and opener/WindowProxy workflows still
  need native popup adoption.
- Provider keys, account sessions and MCP connection secrets use the desktop
  Secret Service (GNOME Keyring or KWallet), without a plaintext fallback.
  Ask can read pages and run the shared fixed recipes in a private WebKitGTK world. Provider requests still require the
  person's model configuration and credentials.
- Voice capture uses `pw-record` from PipeWire, keeping audio in memory for
  cloud transcription. An on-device speech recognizer is not included.

For these desktop integrations install a working `xdg-desktop-portal` with a
FileChooser backend, a Secret Service provider, and PipeWire's `pw-record`.
The tested FileChooser backend is `xdg-desktop-portal-gtk`.

## Remaining platform work

- Work's model orchestration is enabled for testing, but its separately owned
  browser contexts, session transfer, screenshots and resource lifecycle still
  require a Linux adapter. Autonomous Work browsing is not functional yet.
- Chrome extensions are not functional: WebKitGTK has no Chrome WebExtensions
  runtime. Enabling the package-install UI alone cannot supply background
  workers, browser APIs, popups or native messaging. Bitwarden 2026.10.0 is the
  compatibility target; its manifest requires these missing facilities. No
  existing Brave extension storage or vault data is imported.
- User-script support still has the existing match-set and document-idle
  restrictions. This is distinct from a full Chrome extension runtime.
- The global launcher shortcut uses the desktop portal on Wayland and a native
  X11 grab on X11. If global registration is unavailable, the focused-window
  shortcut remains available. The shortcut recorder rebinds both the global
  backend and the focused-window fallback; a portal may require fresh consent.
- Folder uploads, notification click/close synchronization, device capture and
  end-to-end cloud transcription need additional live qualification.
- Linux distribution packages and automatic updates are not qualified by this
  build procedure.

## Verification

The native engine test uses a loopback HTTP fixture and a temporary profile:

```sh
cargo test -p zephium-engine --lib --features agentic-browser \
  native_download_and_isolated_page_reader -- --ignored --test-threads=1
```

It checks downloads, collision numbering, private history, native shutdown,
page reading, and a fixed checkbox action with independent DOM verification.
The Secret Service round-trip test creates and deletes its own test credential:

```sh
cargo test -p zephium-credentials desktop_secret_round_trip \
  -- --ignored --test-threads=1
```

Neither test needs a provider API key or accesses an existing vault secret.
