# Vendored crates patched for Wavelane

Applied through `[patch.crates-io]` in `Cargo.toml`. Each directory is
the published crate source with the changes below; licenses are unchanged.

- `ksni` 0.3.6 (Unlicense): implements KDE's
  `org.kde.StatusNotifierItem.ProvideXdgActivationToken` and passes the token
  to the new `Tray::provide_xdg_activation_token` callback.
- `gpui-pre-linux` 0.3.9 (Apache-2.0; modified), the Linux platform
  `gpui-kit` 0.8.0 runs on (its crate name is `gpui_linux`): adds
  `gpui_linux::set_pending_activation_token`. On Wayland the next new toplevel
  window, or the next `activate` of an existing one, uses that xdg-activation
  token so the compositor raises and focuses the window. Also adds
  `gpui_linux::request_launch_activation_token`: the next `activate` asks the
  compositor for a token from the latest click instead, for a process Wavelane
  starts (the updater's *Restart Wavelane*), so the new window opens in front.

  Also: newer KWin offers blur only through the standard
  `ext_background_effect_v1` protocol, not `org_kde_kwin_blur`; a window
  with `WindowBackgroundAppearance::Blurred` now uses it when present
  (`globals.background_effect`, `update_window`). The Sound map blurs the
  desktop behind it this way.

Both let a tray click bring Wavelane's window to the front on Wayland (KWin's focus
stealing prevention otherwise only marks it as wanting attention). Until
0.1.10 this patch lived in `gpui-fast-linux`, which `gpui-kit` does not use
without its `gpui-fast` feature, so it never reached the windows; verified
now with WAYLAND_DEBUG: a tray click activates with KDE's own token.

- `gpui-component` 0.8.0 (Apache-2.0, Longbridge; modified): the Linux
  client-side window frame (`src/window_border.rs`) rounds all four corners at
  10px instead of drawing a square frame, border and shadow. Wavelane opens its
  window with a transparent root surface and paints its own rounded console,
  so the desktop shows through the corners as on Windows.

- `gpui-fast-linux` 0.1.10 (Apache-2.0; modified), enabled only by Wavelane's
  experimental `gpui-fast` feature. The same activation-token, launch-token and
  standard background-effect patches described above are applied to Fast's
  platform too. The application aliases its direct platform dependency so
  patch API calls always reach the backend actually opening its windows.
  Fast's native composition code remains intact. GPUI Kit's `gpui-fast` feature
  selects a consistent Fast core across Kit, Base, Component and assets;
  upstream `gpui-pre` dependencies still compile because Cargo features are
  additive. The lockfile pins the tested Fast packages at 0.1.10.
