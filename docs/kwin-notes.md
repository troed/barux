# KWin plumbing — what is true on the machine

Facts the KDE integration depends on, each with a status tag. **Everything below
is `UNSURE` until `tools/kwin-probe.sh` has been run on a real Plasma 6 (KWin,
Wayland) session and the answer pasted in.** A tag of `UNSURE` is not a
showstopper: the task that consumes the fact implements the plan's stated
interface, and only that task changes if the answer differs. Each entry names
the task and the command that settles it.

To verify: run the listed stage of `tools/kwin-probe.sh` as your session user,
and paste the observed result into the `Observed:` line, flipping the tag to
`VERIFIED`.

---

## 1. The virtual-desktop interface (consumed by Task 3)

**Claim:** barux reads Plasma's virtual desktops from `org.kde.KWin`
`/VirtualDesktopManager`, interface `org.kde.KWin.VirtualDesktopManager`: a
`desktops` property (one entry per desktop, position order), a `current`
property, and a `currentChanged` signal for switches. A desktop **position**
(1-based) is mapped to its **name**; barux stores `position → name`.

**Status:** UNSURE — the exact property types and the name's source (a `name`
property on each desktop object, a `desktopName`/`desktopDataChanged` signal, or
`desktops` returning `{id, name}` pairs) are not yet confirmed. Task 3 maps
position→name however the introspection shows it, without changing the neutral
`HyprState` shape.

**Verify:** `tools/kwin-probe.sh introspect` (or
`gdbus introspect --session --dest org.kde.KWin --object-path /VirtualDesktopManager`).

Observed:

---

## 2. A window's `internalId` is stable and unique (consumed by Tasks 4, 6, 7)

**Claim:** `workspace.windowList()[i].internalId` is unique among live windows
and stable for the life of a window, so barux can use it as the window identity
(`Window.addr`) and to focus a window later (Task 7). Plasma 6 also has
`internalId` as a UUID string.

**Status:** UNSURE — stability across a window's life (and uniqueness) is
assumed, not yet observed.

**Verify:** `tools/kwin-probe.sh journal` after moving windows around and
watching `internalId` in the snapshots.

Observed:

---

## 3. `callDBus` from the script reaches a session-bus name (consumed by Task 4)

**Claim:** `callDBus("org.barux.KWin", "/KWin", "org.barux.KWin", "Snapshot", json)`
from the KWin script reaches a service name that barux owns on the user's
session bus, calling method `Snapshot(s)`.

**Status:** UNSURE — requires barux (or a listening `gdbus`/`dbus-monitor`) to
own the name and the script to be loaded.

**Verify:** with barux (or `gdbus`/`dbus-monitor`) owning `org.barux.KWin`, run
`tools/kwin-probe.sh install` then `tools/kwin-probe.sh journal` and confirm the
call arrives (and no "callDBus failed" line).

Observed:

---

## 4. A generated script activates a window (consumed by Task 7)

**Claim:** loading a generated one-shot KWin script through
`org.kde.kwin.Scripting` (`loadScript` + `run` + `unloadScript`) — or
`workspace.activeWindow = <window>` inside it — brings a window to the front.
Alternative if this does not work: `org.kde.KWin` `activateWindow`, or a
`kdotool`-style approach.

**Status:** UNSURE — this is the route the plan picks; it is not confirmed.

**Verify:** load a script that sets `workspace.activeWindow` to a chosen
window's object, and watch the focus change.

Observed:

---

## 5. `kglobalaccel` shows/hides the Overview (consumed by Task 7)

**Claim:** invoking the `Overview` component's shortcut on
`org.kde.kglobalaccel` (`Component.invokeShortcut "Overview"`) toggles KWin's
Overview effect, which is what `overview_action` means on KDE.

**Status:** UNSURE — the component/shortcut name and whether invoke toggles.

**Verify:**
`gdbus call --session --dest org.kde.kglobalaccel --object-path /component/... --method org.kde.kglobalaccel.Component.invokeShortcut Overview`
(or `qdbus6 org.kde.kglobalaccel …`); see `tools/kwin-probe.sh introspect` notes.

Observed:

---

## 6. Install + enable + `reconfigure` loads the script (consumed by Tasks 4, 10)

**Claim:** copying `dist/kwin-script/` to
`~/.local/share/kwin/scripts/org.barux.desktop/`, setting
`[Plugins] org.barux.desktopEnabled=true` in `kwinrc`, and calling
`org.kde.KWin` `/KWin` `reconfigure` makes KWin load the script (its
`console.log` then appears in the KWin journal). Uninstalling the directory and
clearing the key, then reconfiguring, stops it.

**Status:** UNSURE — assumed from the documented KWin script packaging.

**Verify:** `tools/kwin-probe.sh install`, then `journal`; then `uninstall` and
confirm `kwinrc` no longer mentions `org.barux.desktop` and the output stops.

Observed:
