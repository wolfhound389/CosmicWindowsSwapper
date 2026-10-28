# CosmicWindowsSwapper

A Windows/KDE-style app switcher for the [COSMIC desktop](https://system76.com/cosmic) on
Pop!_OS — replacing the default vertical Super+Tab list with a centered panel of live window
previews and titles (the classic icon strip is still available as a layout option).

> **Renamed project:** this used to be `cosmic-ext-app-switcher`. The binaries are now
> `cosmic-windows-swapper` and `cosmic-applet-windows-swapper`, and the app id is
> `io.github.wolfhound389.CosmicWindowsSwapper`. Your existing settings are copied over
> automatically on first launch.

<table align="center">
  <tr>
    <td align="center"><img src="docs/screenshot-overlay.png" alt="App switcher overlay"/></td>
    <td align="center"><img src="docs/screenshot-applet.png" alt="Panel applet"/></td>
  </tr>
  <tr>
    <td align="center"><em>Super+Tab switcher overlay</em></td>
    <td align="center"><em>Panel applet — theme &amp; shortcut toggle</em></td>
  </tr>
</table>

---

## Install

**One-line (no Rust required):**

```bash
curl -fsSL https://raw.githubusercontent.com/wolfhound389/CosmicWindowsSwapper/main/install.sh | bash
```

**Or build from source:**

```bash
# Install dependencies (one-time)
sudo apt install libxkbcommon-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source ~/.cargo/env

git clone https://github.com/wolfhound389/CosmicWindowsSwapper
cd CosmicWindowsSwapper
make install
```

Changes take effect immediately — no logout required.

> ### Do not install this from the COSMIC Store
>
> The store listing is broken and cannot be made to work. cosmic-comp only offers
> `zcosmic_toplevel_info_v1`, `zcosmic_toplevel_manager_v1` and `zwlr_layer_shell_v1` to
> clients without a Wayland security context, and Flatpak always attaches one, so a
> sandboxed build can neither list your windows nor draw the overlay. Super+Tab silently
> does nothing. Measurements are in
> [#10](https://github.com/wolfhound389/CosmicWindowsSwapper/issues/10); System76 confirmed
> there is no path for it today in
> [cosmic-comp#2734](https://github.com/pop-os/cosmic-comp/issues/2734#issuecomment-5293857795), and are planning
> configurable `cosmic-session` components to allow it in future.
>
> Removal of the listing is requested in
> [cosmic-flatpak#259](https://github.com/pop-os/cosmic-flatpak/issues/259). Until that
> lands it is still installable, and still serves 0.1.3. **Use the install script above
> instead.**
>
> If you already installed it from the store, uninstalling leaves Super+Tab dead, because
> the shortcut it registered outlives the app and COSMIC does not fall back to its built-in
> switcher. To clean up:
>
> ```bash
> flatpak uninstall io.github.wolfhound389.CosmicWindowsSwapper.applet
> curl -fsSL https://raw.githubusercontent.com/wolfhound389/CosmicWindowsSwapper/main/uninstall.sh | bash
> ```
>
> The second command rewrites COSMIC's shortcut config and restores the default switcher.
> It is safe to run whether or not the native version is installed.

---

## Uninstall

```bash
curl -fsSL https://raw.githubusercontent.com/wolfhound389/CosmicWindowsSwapper/main/uninstall.sh | bash
```

Or from the project directory:

```bash
make uninstall
```

---

## Usage

| Shortcut | Action |
|---|---|
| Super+Tab / Alt+Tab | Open switcher / cycle forward |
| Super+Shift+Tab / Alt+Shift+Tab | Cycle backward |
| Modifier release | Activate selected window |
| Escape | Cancel — return to original window |
| Click a preview | Select and activate it |
| Click the ✕ button | Close that window without switching to it |
| Super+Q / Alt+Q | Close the highlighted window and keep the switcher open |

### Layouts

Three layouts ship, all fully themable:

- **Grid** (default) — the Windows/KDE-style block of live window thumbnails with titles, arranged roughly square on screen.
- **Single line** — the same previews on one horizontal line. When there are more windows than fit across the screen it becomes a sliding window: holding the switcher key and pressing Tab scrolls the row, hiding the cards that move off the left edge and revealing the next ones, and the cycle wraps around endlessly until you pick a window or let go of the hotkey.
  The row has two modes, picked under **Row behaviour** in the applet:
  - **Slide** — the window scrolls only once the selection reaches the right edge.
  - **Carousel** — the selection stays centred and every focus change shifts the whole row by one card, so the leftmost window slides off screen and a new one enters from the right. Motion is constant and easy to follow whichever card you are on.
- **Icons** — the original macOS-style horizontal strip of app icons.

The overlay is always centred on the screen. Every time focus moves to another window
the new card animates in, so the hand-off between two cards is continuous. The effect
replays on every focus move, not just the first.

### Animation

Pick the effect under **Animation** in the applet. The choice is a dropdown because the
names need room, and **Off** is a real entry rather than a zero-length duration — nothing
moves, the newly selected card appears instantly, and pressing Enter or Super+Q after a
single Tab acts immediately instead of waiting out a tween.

- **Zoom** (default) — the incoming card springs past its resting size and settles back,
  while the card losing focus glides down. This is the Windows Alt+Tab / macOS ⌘+Tab
  behaviour, and it stays the default so upgrading changes nothing.
- **Fade** — the incoming card fades up from 55% opacity. No geometry changes, so the row
  cannot reflow. Familiar from GNOME and KDE.
- **Slide** — the card enters from 26 px to its left. Most at home in the single-line
  layout; KDE's slideshow and macOS Stage Manager use similar motion.
- **Zoom + fade** — both at once, the softened combination COSMIC and Android use.
- **Off** — no animation at all.

**Speed** scales the duration from x0.5 to x5 in half steps. It is hidden while the
animation is Off, because there is nothing left to speed up.

Everything outside the cards is a real frosted-glass backdrop: the compositor blurs the
desktop behind the overlay (`ext_background_effect_manager_v1`) and the switcher lays a
light dim over it, so the cards stay legible over a busy wallpaper while never covering
the desktop completely. Your theme's background colour supplies the tint, and
**Blur strength** in the applet sets how heavily that dim sits — 0% leaves the blurred
desktop untouched, 100% hides it behind an opaque sheet.

Long window titles wrap onto up to three lines instead of being clipped, and every card is
sized to the tallest title in the current set so all previews stay exactly the same size.
Every preview card carries a Material-style close button (a cross in a circle) in its
top-right corner for closing a window directly from the switcher.

Switch between them from the panel applet. **Preview size** (S / M / L), a
**Show window titles** toggle and a **Show close buttons** toggle sit next to it; every other knob — gap, padding, corner
radius, selection ring, column count, icon visibility — lives in the `style` section of
the config file:

```ron
style: (layout:Grid,thumb_width:300,thumb_height:170,gap:14,padding:16,columns:0,row_mode:Slide,anim_speed:1.0,anim_style:Zoom,blur_strength:0.20,enabled:true,show_titles:true,show_icons:true,radius:10.0,border:2.5,show_close:true),
```

Set `columns` to a number between 1 and 5 to pin the grid width; `0` picks it from the
window count (and `layout: Row` keeps everything on a single line, scrolling when the
windows no longer fit). `row_mode` is `Slide` or `Carousel`, and `anim_speed` scales the
animation anywhere from `0.5` to `5.0` (values outside that range are clamped).
`anim_style` is `Off`, `Zoom`, `Fade`, `Slide` or `ZoomFade`, and `enabled: false` stops
the switcher from appearing at all while leaving its shortcut in place. Set
`show_close: false` to hide the close buttons.

Fields missing from a config file fall back to their defaults, so a file written by an
older release — without `anim_style` or `enabled` — loads with that user's existing layout
intact.

`blur_strength` is the opacity of the backdrop dim, from `0.0` (blurred desktop, no
darkening) to `1.0` (opaque). It defaults to `0.20`, which keeps the blurred shapes
readable as glass instead of flat grey. The compositor's blur radius is fixed by the
protocol — this knob controls the veil drawn on top of it, not the blur itself.

---

## Panel applet

> **Tip:** After installing, right-click your COSMIC panel → **Edit Panel** → **Add Applet** → search for **Windows Swapper Settings** and add it. This gives you a one-click toggle and theme picker directly in your panel.

Add the **Windows Swapper Settings** applet to your COSMIC panel for quick access to:
- **Show window switcher** — turn the overlay off entirely without touching the terminal
- **Super+Tab shortcut** toggle — unregister the hotkey while keeping the applet
- **Theme picker** — Dark, Light, Frosted, or Midnight
- **Layout** — preview grid, single line, or icon strip, preview size, titles, close buttons
- **Row behaviour** — Slide or Carousel, shown for the single-line layout
- **Animation speed** — x0.5 to x5 in half steps
- **Blur strength** — 0% to 100% in 10% steps, the dim laid over the blurred backdrop

## Enable / Disable

Via the panel applet toggle, or from the terminal:

```bash
make enable         # register as COSMIC window switcher (live reload)
make disable        # remove — restores COSMIC's default switcher
make status         # show current state
make check-compat   # verify COSMIC environment is compatible
```

---

## How it works

COSMIC's compositor (`cosmic-comp`) lets you override the `WindowSwitcher` action by pointing a config file at any binary. The scripts detect the config path automatically — if COSMIC updates and moves to a new config version, it still works.

Each key press launches the binary fresh. The first invocation creates a layer-shell overlay and binds a Unix socket. Subsequent presses (while the modifier is held) connect to that socket and send a `next`/`prev` signal — the running instance advances the selection and re-renders. Releasing the modifier triggers window activation via `zcosmic_toplevel_manager_v1`.

**Wayland protocols used:**
- `zcosmic_toplevel_info_v1` — enumerate open windows
- `zcosmic_toplevel_manager_v1` — activate a window
- `zwlr_layer_shell_v1` — overlay surface centered on screen

---

## Project structure

```
src/
  main.rs      # entry point: IPC check, then launch app
  app.rs       # libcosmic Application: state machine, socket subscription
  ui.rs        # horizontal icon strip view
  wayland.rs   # background thread: enumerate toplevels, activate window
  icons.rs     # app_id → icon name via .desktop file lookup
scripts/
  find-config.sh   # detects COSMIC shortcuts config path dynamically
  enable.sh        # writes WindowSwitcher entries to COSMIC config
  disable.sh       # removes them
  status.sh        # prints current state
install.sh     # standalone installer (downloads binary, no Rust needed)
uninstall.sh   # standalone uninstaller
.github/
  workflows/
    release.yml    # builds x86_64 + aarch64 binaries on tag push
```

---

## Releasing a new version

```bash
git tag v0.2.0
git push origin v0.2.0
```

GitHub Actions will build binaries for x86_64 and aarch64 and publish a release automatically.

---

## Requirements

- Pop!_OS with COSMIC desktop
- x86_64 or aarch64 architecture

Building from source additionally requires `libxkbcommon-dev` and Rust.

---

## Credits

This project was inspired by [cosmic-app-switcher](https://github.com/j0rdiun/cosmic-app-switcher),
a COSMIC app switcher by Jordan, and began as a fork of it. The original macOS CMD+Tab-style
switcher, the theme presets, the first version of the panel applet and the distribution tooling
all come from that work, and GPL-3.0 requires that this is said plainly.

Development since the rename to CosmicWindowsSwapper — live window previews, the grid, single-line
and icon layouts, the carousel, the blurred backdrop, per-card animation and this settings panel —
happened here.

Thanks are due to that original software and its author.

---

## License

GPL-3.0-only — see [LICENSE](LICENSE)
