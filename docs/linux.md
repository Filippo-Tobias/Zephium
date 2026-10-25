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

## Platform limitations

- Native Work browser execution and the Ask panel's page assistant require
  macOS or Windows. The Linux build reports the page assistant as unavailable.
- The global launcher shortcut uses the desktop portal on Wayland and a native
  X11 grab on X11. If global registration is unavailable, the focused-window
  shortcut remains available. The shortcut recorder is unavailable on Linux.
- Extension support is restricted by the existing Linux user-script backend;
  document-idle scheduling and per-frame match enforcement are unavailable.
- Linux distribution packages and automatic updates are not qualified by this
  build procedure.
