# KDE (KWin + Plasma) support for barux — design

Date: 2026-10-17
Branch: `kde`
Status: approved by the human partner; ready to plan

## 1. Why

barux is a Rust daemon for the Touch Bar of Apple Silicon MacBook Pros on Asahi
Linux; it replaces tiny-dfr and draws the bar pixel by pixel. Its author runs
**Hyprland**, and the parts of barux that are desktop-aware are written against
Hyprland: the `workspaces` item (workspace capsules, the icon strip of each
workspace's windows, and the per-app buttons those icons unfold), the `app_keys`
item (buttons chosen by the focused window's class), the `hyprctl` action type,
and the environment barux builds for the commands it launches.

This machine runs **KDE Plasma 6 on Wayland (KWin)** on a 13" M1 MacBook Pro with
a Touch Bar — the same class of hardware the project targets — and the desktop
features are simply inert here. The feature the human partner wants is the one
shown in the author's video: the workspace capsules with the icon strip of
running windows, and the ability to tap an app's icon and get its own buttons.
On top of that, one KDE-facing feature that does not exist yet: Plasma
notifications on the bar. And one correction: the example configuration's
virtual `esc` button is dead space on a machine whose Touch Bar sits to the right
of a physical Esc key.

`main` stays the clone of upstream (`github.com/idrode/barux`) and keeps the
Hyprland implementation, so upstream work can still be merged forward. The KDE
work lives on the `kde` branch.

## 2. Goals

- **`workspaces` on KWin**: desktop capsules (`style = "dots"` and `"capsule"`),
  the window-icon strip with all its `strip` modes (`active`, `pressed`, `all`,
  `none`, `tap`), the `strip = "tap"` levels, the per-app buttons
  (`app_actions`) and the `overview_action` drag. Same configuration surface and
  same drawing code; only the source of desktop/window state and the actions
  change.
- **`app_keys` on KWin**: the fixed slot whose button set follows the focused
  window's class.
- **A KDE action type** to replace `hyprctl` for KDE users: switch desktop,
  activate a window, launch an application, and toggle KWin's Overview effect.
- **A `notifications` item** (new): a mirror of Plasma notifications — a bell in
  the freed left corner, unfolding into a strip of chips, each chip showing the
  notifying app's icon and name, tapping a chip dismissing that notification for
  real.
- **A Plasma session environment** for launched commands (the equivalent of the
  Hyprland-specific block in `src/user.rs`).
- **KDE-facing configuration and docs**: `config.example.toml` recipes moved from
  Hyprland tooling to KDE tooling, and the virtual `esc` button dropped from the
  example.
- **Hyprland code kept in the tree** on this branch, so `main` keeps merging
  cleanly.

## 3. Non-goals

- **Plasma Activities.** The analogue of a Hyprland workspace here is a virtual
  desktop, not an Activity. Activities are out of scope.
- **Replacing plasmashell as the notification server.** barux mirrors; it never
  owns `org.freedesktop.Notifications`.
- **A user-facing desktop switch.** There is no `desktop = "kde"` setting to
  choose between Hyprland and KDE as supported modes. The Hyprland modules stay
  in the tree for mergeability, not as a second supported configuration.
- **X11 sessions.** Plasma 6 on Wayland only.
- **Changing the DRM, touch, render, volume, brightness, battery or idle code**
  beyond what desktop integration strictly needs.

## 4. Anti-goals

Things that would make this a failure even if it technically worked:

- **The bar becoming unusable when KWin or Plasma is absent or restarts.** The
  project's rule is that an unavailable desktop means the affected item is not
  drawn and takes no room, with no error; the rest of the bar is unaffected. A
  blank bar, a panic, or a stuck redraw is a failure.
- **Losing a notification.** Mirroring must never suppress, swallow or delay
  something Plasma would have shown. barux is a viewer.
- **Polling.** The project draws nothing and burns no CPU when idle; every
  desktop source must be event-driven (D-Bus signals, a KWin script's callbacks,
  the existing epoll loop). A timer that wakes to re-query KWin is a failure.
- **Making `main` painful to merge.** Shared files should be touched additively;
  the Hyprland modules should not be rewritten to make room.

## 5. Constraints and environment

- **Plasma 6 on Wayland**, KWin, on Asahi Arch, ARM64.
- **13" M1 MacBook Pro**: Touch Bar is a ~2170×60 DRM display plus an evdev
  digitizer; a **physical Esc key and Touch ID sit to the left of the bar**, so
  the leftmost region of the Touch Bar is not where Esc belongs on this machine.
- **The Touch Bar has a single owner.** `tiny-dfr` must be stopped before barux
  runs; there is no sharing.
- **barux may run as root** (installed as a system service) but must speak to the
  **user's** session: their D-Bus session bus, their Wayland socket, their
  runtime directory. The project already builds that environment for launched
  commands; this design extends the same idea to barux's own connections.
- **Rust**, edition 2024, existing dependency set. A D-Bus client is the one
  genuinely new dependency class (crate choice left to the builder). No `unsafe`
  unless necessary; no `unwrap()` on normal paths.
- **Installability**: whatever KWin-side artifact the design needs must be
  installed and registered by `install.sh` and removed by `uninstall.sh`;
  the user should not have to hand-edit `kwinrc` or the KDE settings.

## 6. What the human partner decided

These are settled; do not re-litigate them.

- The work goes on a **`kde` branch**; `main` keeps upstream's Hyprland
  implementation and **the Hyprland code stays in the tree** on the branch, for
  mergeability.
- Keep the name **`barux`**, the unit **`barux.service`**, and the paths
  **`/etc/barux`** and **`/var/lib/barux`**. No renaming.
- The **workspaces strip and per-app buttons are the centerpiece** (the video).
- A **`notifications` item** is wanted.
- Notifications are a **mirror, not a takeover**: Plasma's own popups and
  notification centre stay exactly as they are.
- The bell sits **where the example config's `esc` is today**, is always visible,
  and shows no number when there is nothing new.
- A strip chip shows the **app icon and the app name**.
- Tapping a chip sends a **real dismiss** (`CloseNotification`), so the bar and
  Plasma's notification centre stay in step.
- Opening the strip **marks the arrivals seen**; the strip shows what fits and
  drops the oldest.
- The virtual `esc` button is **dropped from the KDE example config** (the
  physical key exists).

## 7. Shape of the work

The project's own rule applies: one verifiable step at a time, each testable on
its own, and everything that can be previewed with `--png` should be. A proposed
order (the plan may refine it):

1. **The desktop seam.** Introduce the single place where a desktop-specific
   source is chosen, without disturbing the Hyprland modules. Keep it small: the
   point is to keep `main` mergeable, not to design a plugin system.
2. **Desktop/window state into `workspaces`.** Virtual desktops from KWin, the
   window list and focus from the KWin script, feeding the existing state
   machine and drawing code unchanged.
3. **Workspaces actions**: switch desktop, activate a window, `overview_action`.
4. **`app_keys`** focus class from the same source.
5. **The KDE action type** (the `hyprctl` sibling).
6. **The Plasma session environment** for launched commands.
7. **The `notifications` item**: bus watch, model, bell, strip, dismiss.
8. **Packaging**: example config, guide, `install.sh`/`uninstall.sh` and the
   KWin script, plus the `main`-facing documentation.

## 8. What is left to the builder

These are deliberate openings; a good builder settles them without asking.

- **D-Bus access.** The crate, and how barux as root reaches the user's session
  bus and KWin (a direct connection, or a small per-user helper alongside the
  existing "run as the session user" machinery).
- **The KWin script.** Its content, how it is registered so KWin loads it at
  session start, and how it reports to barux — the natural route is a D-Bus
  method call on a name barux registers on the session bus. Decide between one
  persistent script and on-demand injection (the `kdotool` crate does the
  latter; a daemon that wants live state wants the former).
- **Which KWin API surface** to use, and therefore the Plasma version floor. Note
  that KWin exposes **no window list over D-Bus by itself**, which is why a
  script is involved at all.
- **Where the desktop list comes from**: `org.kde.KWin.VirtualDesktopManager`
  (properties and `currentChanged`) or the script, for uniformity.
- **The desktop seam's exact shape** (a trait, an enum, or something simpler),
  judged by how little it disturbs `main`.
- **The KDE action type's schema** and what to call it.
- **Icon lookup for notifications**: reuse the existing icon machinery and the
  `.desktop`/`StartupWMClass` lookup the window strip already uses.
- **Fallbacks** when KWin or Plasma is unreachable, per the anti-goals.
- **What happens to the workspace birth/closing animations** (`wsbirth.rs`).
  Hyprland creates and destroys workspaces constantly; Plasma's virtual desktops
  are largely static, so that code path will rarely fire. Keeping it as-is is
  acceptable; the active-change and press animations definitely still apply.

## 9. Risks

- **Root and the session bus.** Whether a root-owned service can connect to the
  user's session bus, become a monitor on it, and call `CloseNotification`; if
  not, the work moves to a per-user helper. Verify on the machine early.
- **Notification mirroring mechanics.** The intended route is to watch the
  session bus the way `dbus-monitor` does (a monitor connection, which cannot
  itself send messages, so a second connection is needed for dismissals). This
  must be confirmed against the real machine before it is built on. Also to
  settle then: notifications that arrive **while Plasma is in Do Not Disturb**
  (the bar should not announce what the desktop is suppressing), `replaces_id`,
  actions, images and `urgency` hints, and whether the bar needs a cap on how
  many it remembers.
- **Activating windows.** KWin may not let a non-plasmashell client activate
  another client's window over D-Bus; the KWin script is the likely answer, but
  confirm.
- **KWin scripting API churn** between Plasma releases, and the version floor it
  implies.
- **Semantic mismatch.** Hyprland has dynamic workspaces with ids including
  negatives and a scratchpad; Plasma has an ordered, mostly static set of virtual
  desktops. `show = "existing"` / `"all"`, the "+N" overflow, and the birth
  animations all need a moment's thought in that light.

## 10. Definition of done

- The example configuration, run through `--png`, draws the workspaces capsules,
  the icon strip and the notification bell without a desktop present.
- On the machine, with a Plasma session: switching virtual desktops from the bar
  works; the strip shows each desktop's windows with the right icons; tapping an
  icon focuses it and unfolds its app's buttons, and those buttons send their
  shortcuts; `app_keys` follows the focus; a posted notification raises the bell
  and tapping its chip dismisses it in Plasma's centre too.
- With KWin or Plasma absent, those items vanish without error and the rest of
  the bar is unaffected.
- `install.sh` puts the KWin script in place and removes it on uninstall.
- The guide documents the KDE behaviour alongside the existing Hyprland text, and
  `main` still merges into the branch without conflict in the Hyprland modules.
