# KWin desktop integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make barux's desktop-aware items — the `workspaces` capsules with their window-icon strip and per-app buttons, and the `app_keys` slot — work under KDE Plasma 6 (KWin) instead of Hyprland.

**Architecture:** A new desktop source (`src/kwin.rs`) replaces the Hyprland source on the live path and produces the *same* state struct (`hypr::HyprState`), so everything downstream (`WsView::from_state`, the `workspaces` state machine, `appkeys`, drawing) is untouched. Virtual desktops come from KWin's D-Bus `org.kde.KWin.VirtualDesktopManager`; the window list and focus come from a small KWin script that pushes snapshots to a barux-owned session-bus name; window activation is done by loading a generated one-shot KWin script. The Hyprland modules stay in the tree, compiling, off the live path.

**Tech Stack:** Rust (edition 2024, no async runtime), `zbus` for D-Bus on a dedicated thread woken into the existing epoll loop (the same shape as `src/gifpick.rs`), a KWin script package under `dist/`, and the existing `nix`/`wayland-client` stack.

**Spec:** `docs/superpowers/specs/2026-10-17-kde-support-design.md`

**Scope:** this plan is the desktop integration — §7 steps 1–6 and 8 of the spec. The `notifications` item (§2, §7 step 7) is a **second plan**, written after this one; it reuses the `Bus` built in Task 2. Nothing here touches notifications, and the spec's notification decisions are not implemented by any task below.

## Global Constraints

- Plasma 6 on Wayland (KWin). No X11 path.
- barux may run as root but must speak to the *user's* session bus, Wayland socket and runtime dir.
- The bar never draws and burns no CPU when idle: every desktop source is event-driven; no polling timers.
- An unavailable desktop means the affected item is **not drawn and takes no room**, with no error; the rest of the bar is unaffected.
- Notifications are a mirror, never the owner of `org.freedesktop.Notifications`. (Out of scope for this plan.)
- Keep `barux`, `barux.service`, `/etc/barux`, `/var/lib/barux`. No renaming.
- The Hyprland modules (`hypr.rs`, `hyprwatch.rs`, `hyprctl.rs`) stay in the tree, declared and compiling; they are not on a live path.
- No `unsafe` unless necessary and commented; no `unwrap()` on normal paths; errors with `anyhow` in the binary.
- Small, verifiable steps; each task ends in a commit.
- Tests must not touch the Touch Bar hardware or the developer's real session.

## Review Focus

These are the inputs and conditions the spec implies but no single task's tests naturally cover. Each is pinned by a test in the task that owns the code, named in brackets.

- **A Plasma session where KWin is restarting, or the script has not loaded yet** — the bar must show no desktop items and no error, then show them on its own once the script reports, exactly as it does today when Hyprland starts late. [Task 2, Task 4]
- **A window whose KWin `resourceClass` is empty** — the strip must still draw it with its letter circle, not a blank cell or a panic. [Task 6]
- **A virtual desktop added or removed while the bar is showing desktops** — the capsules must follow without a birth animation misfiring, because Plasma's desktops are added rarely where Hyprland's are created constantly. [Task 3]
- **A window that closes leaving its desktop empty** — the strip must drop the icon and the capsule must stay, rather than the item folding or the workspace being absorbed. [Task 4]
- **Two windows of the same class on different desktops** — the strip must attribute each to the right capsule; KDE reports desktops per window, and that mapping is where the two desktops differ most. [Task 6]

---

### Task 1: Verify the KWin plumbing on the machine (spike)

Before building on any KWin interface, prove each one on the real Plasma session and record what is true. The deliverable is the KWin script skeleton plus a written record; the spike code is kept, not thrown away.

**Files:**
- Create: `dist/kwin-script/metadata.json`
- Create: `dist/kwin-script/contents/code/main.js`
- Create: `tools/kwin-probe.sh`
- Create: `docs/kwin-notes.md`

**Interfaces:**
- Produces: the confirmed facts later tasks depend on — the exact object path and property signatures of the virtual-desktop interface; whether a window's `internalId` is stable and unique; whether `callDBus` from the script reaches a session-bus name; whether the generated-script route activates a window; whether `kglobalaccel` shows/hides the Overview; whether installing to `~/.local/share/kwin/scripts/<id>/`, enabling in `kwinrc`, and calling `org.kde.KWin` `reconfigure` makes KWin load it. Each fact is written to `docs/kwin-notes.md` with a VERIFIED/UNSURE tag.

- [ ] **Step 1: Write the probe script**

`tools/kwin-probe.sh` runs, as the session user: `qdbus6`/`gdbus` introspections of `org.kde.KWin` `/VirtualDesktopManager` and `/KWin`; `dbus-monitor` on `org.kde.KWin` for one desktop switch; a listing of a running KWin script's `console.log` output from the journal; and the install/enable/uninstall sequence of `dist/kwin-script/`.

- [ ] **Step 2: Write the minimal KWin script**

`dist/kwin-script/contents/code/main.js` connects `workspace.windowAdded`, `windowRemoved`, `windowActivated`, `currentDesktopChanged` and each window's `desktopsChanged`, and on each prints a full snapshot (`workspace.windowList()` mapped to `{ internalId, resourceClass, resourceName, desktopFileName, caption, desktops, active }`) plus the current desktop list. It calls `callDBus("org.barux.KWin", "/probe", "org.barux.KWin", "Snapshot", JSON.stringify(snapshot))`.

- [ ] **Step 3: Run the probe and record results**

Run: `tools/kwin-probe.sh` on the machine. Expected: the desktops, the window snapshots, and the result of each question in the Interfaces block above. Write every answer into `docs/kwin-notes.md`, tagged VERIFIED or UNSURE. Anything UNSURE changes only the task that uses it, and the note names which.

- [ ] **Step 4: Confirm the install/enable/uninstall round trip**

Run: the probe's install and uninstall stages. Expected: after enabling, `journalctl -b --user-unit plasma-kwin_wayland.service` (or the KWin debug console) shows the script's output; after uninstall and `reconfigure`, it stops, and `kwinrc` no longer mentions it.

- [ ] **Step 5: Commit**

```bash
git add dist/kwin-script tools/kwin-probe.sh docs/kwin-notes.md
git commit -m "kwin: probe the KWin plumbing and record what is true"
```

---

### Task 2: The desktop seam

Introduce the single place where the desktop source lives, change nothing observable, and keep the Hyprland modules compiling and intact. After this task the daemon runs exactly as before except that the desktop source is a KWin one that is not yet connected.

**Files:**
- Create: `src/kwin.rs`
- Create: `src/dbus.rs`
- Modify: `src/main.rs` (the `App` struct, `new_app`, `watch_hypr` and the loop arms it touches)
- Modify: `src/desktopidle.rs` only if its Hyprland-specific idle source must be re-pointed; otherwise leave it
- Test: `src/kwin.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::hypr::{HyprState, Window, ReadOutcome}` (reused as the neutral desktop state; see the note below).
- Produces:
  - `src/dbus.rs`: `pub struct Bus` (a `zbus` connection on its own thread), `pub fn connect(env: &[(String,String)]) -> Result<Bus>`, `pub fn fd(&self) -> BorrowedFd<'_>` (the wake pipe for epoll), `pub fn drain(&mut self)`, `pub fn call(&self, dest: &str, path: &str, iface: &str, method: &str, args: &[zbus::zvariant::Value<'_>]) -> Result<zbus::zvariant::OwnedValue>`.
  - `src/kwin.rs`: `pub struct Kwin` with `pub fn new(uid: Option<u32>) -> Kwin`, `pub fn event_fd(&self) -> Option<BorrowedFd<'_>>`, `pub fn connect(&mut self) -> Result<()>`, `pub fn connected(&self) -> bool`, `pub fn read(&mut self) -> ReadOutcome`, `pub fn disconnect(&mut self)`, `pub fn session_env(&self) -> Option<crate::user::HyprEnv>` (Task 9 renames that type to `DesktopEnv`), and `pub state: crate::hypr::HyprState`.

**Design note (a call this plan makes):** the neutral desktop state stays `hypr::HyprState`. `WsView::from_state`, the `workspaces` state machine, `appkeys`, `appactions` and `scenes` are then untouched, which is what keeps `main` mergeable. `src/kwin.rs` imports the type under a neutral name (`use crate::hypr::{HyprState as DesktopState, Window, ReadOutcome};`) rather than renaming it anywhere.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn without_a_desktop_workspaces_take_no_room() {
    // A Kwin that never connected reports no state, so the item is filtered out.
    let k = Kwin::new(None);
    assert!(!k.connected());
    assert!(k.state.workspaces.is_empty());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib kwin::tests::without_a_desktop_workspaces_take_no_room`
Expected: FAIL — `Kwin` not defined.

- [ ] **Step 3: Implement `src/kwin.rs` and `src/dbus.rs`**

`Bus` follows `src/gifpick.rs`'s shape: a `std::thread` owning the `zbus` connection, a self-pipe/`eventfd` written whenever a reply or signal arrives so the epoll loop can wake, and a `Mutex`-guarded reply queue. `Kwin` holds an `Option<Bus>`, an empty `HyprState`, and a `preferred_uid: Option<u32>` used to build the session environment for the bus address (reuse `crate::user::session_env`).

- [ ] **Step 4: Swap the source in `main.rs`**

Replace the `hypr: Hypr` field with `desktop: Kwin`, and re-point the call sites the interface map lists (`main.rs:2108-2109, 2280, 2442, 2464-2471, 2947, 2955, 2964, 2975`) to `desktop`. Rename `watch_hypr` to `watch_desktop` and `TOKEN_HYPR`/`TOKEN_HYPR_RETRY`/`TOKEN_HYPR_WATCH`/`TOKEN_HYPR_MOUNTS` to `TOKEN_DESKTOP*`. Leave `hypr.rs`/`hyprwatch.rs`/`hyprctl.rs` in the module list and compiling.

- [ ] **Step 5: Run the suite**

Run: `cargo test` and `cargo clippy --all-targets`
Expected: PASS. Tests that referenced `App.hypr` are updated to `App.desktop`; the `fake_hypr` preview mechanism is kept as-is (Task 5 re-points it).

- [ ] **Step 6: Commit**

```bash
git add src/kwin.rs src/dbus.rs src/main.rs
git commit -m "kwin: introduce the desktop seam over the Hyprland source"
```

---

### Task 3: Session bus and the virtual desktop list

Read Plasma's virtual desktops from D-Bus and follow changes, with no polling. This is the first task whose output is visible on the bar: `show = "all"` begins to work.

**Files:**
- Modify: `src/kwin.rs`
- Modify: `src/dbus.rs`
- Test: `src/kwin.rs`

**Interfaces:**
- Consumes: `Bus` (Task 2).
- Produces: inside `Kwin`, `fn read_desktops(&mut self) -> Result<bool>` and a D-Bus signal subscription resolving the `currentChanged`/`desktopDataChanged` signals into `state.workspaces: BTreeMap<i64, String>` (KDE desktop **position** → name) and `state.active_workspace: Option<i64>`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn virtual_desktops_become_workspaces_by_position() {
    // A fake service answers the desktops property with two desktops and current 2.
    let mut k = kwin::tests::kwin_on(fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2));
    assert_eq!(k.state.workspaces, BTreeMap::from([(1, "Uno".into()), (2, "Dos".into())]));
    assert_eq!(k.state.active_workspace, Some(2));
}

#[test]
fn a_desktop_switch_arrives_without_a_timer() {
    let mut k = kwin::tests::kwin_on(fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2));
    kwin::tests::emit_current_changed(&k, 1);
    assert_eq!(k.read(), ReadOutcome::Changed);
    assert_eq!(k.state.active_workspace, Some(1));
}

#[test]
fn a_desktop_appears_and_disappears_between_reads() {
    let mut k = kwin::tests::kwin_on(fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2));
    assert_eq!(k.read(), ReadOutcome::Changed);
    kwin::tests::set_desktops(&k, &[(1, "Uno")], 1);
    assert_eq!(k.read(), ReadOutcome::Changed);
    assert_eq!(k.state.workspaces.len(), 1);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib kwin::tests`
Expected: FAIL — no `read_desktops`, no test bus helper.

- [ ] **Step 3: Implement the desktop read and the signal subscription**

Add a `#[cfg(test)] pub mod tests` in `src/kwin.rs` with `kwin_on(bus)`, `fake_bus_with(desktops, current)` and `emit_current_changed`, mirroring `hypr::fake::FakeHypr`'s spirit (a real private bus, not a mocked API) — the fake is a small `zbus` service exporting `/VirtualDesktopManager`. Skip the tests (with a printed note) when `dbus-daemon` is not on `PATH`, the way the font-dependent scene tests skip.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test --lib kwin::tests -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/kwin.rs src/dbus.rs
git commit -m "kwin: read and follow the virtual desktop list over D-Bus"
```

---

### Task 4: The KWin script bridge

Bring in the window list and the focused window from the installed KWin script, live. This is the data the strip has been waiting for.

**Files:**
- Modify: `src/kwin.rs`
- Modify: `dist/kwin-script/contents/code/main.js`
- Test: `src/kwin.rs`

**Interfaces:**
- Consumes: `Bus` (Task 2); the snapshot shape fixed in Task 1's `docs/kwin-notes.md`.
- Produces: barux registers the session-bus name `org.barux.KWin` with the object `/KWin` and interface `org.barux.KWin`, method `Snapshot(s: String)`. A snapshot is a JSON object `{ "desktops": [ { "position": i64, "name": String } ], "current": i64, "windows": [ { "id": String, "class": String, "title": String, "desktop": i64 } ], "focused": String|null }`. `Kwin::read()` folds a snapshot into `state`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_snapshot_becomes_windows_on_their_desktops() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    kwin::tests::push_snapshot(&k, r#"{"desktops":[{"position":1,"name":"Uno"},{"position":2,"name":"Dos"}],
        "current":2,"windows":[{"id":"a1","class":"kitty","title":"htop","desktop":1},
        {"id":"a2","class":"zen","title":"Mail","desktop":2}],"focused":"a2"}"#);
    assert_eq!(k.read(), ReadOutcome::Changed);
    assert_eq!(k.state.window_count(1), 1);
    assert_eq!(k.state.focused_window().map(|w| w.class.as_str()), Some("zen"));
    assert_eq!(k.state.windows[0].workspace, 1);
}

#[test]
fn a_window_with_no_class_still_arrives() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    kwin::tests::push_snapshot(&k, r#"{"desktops":[{"position":1,"name":"Uno"}],"current":1,
        "windows":[{"id":"a1","class":"","title":"","desktop":1}],"focused":null}"#);
    assert_eq!(k.read(), ReadOutcome::Changed);
    assert_eq!(k.state.windows[0].class, "");
}

#[test]
fn a_window_that_closes_leaves_its_desktop_in_place() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    kwin::tests::push_snapshot(&k, snapshot_one_window_on_desktop_one());
    assert_eq!(k.read(), ReadOutcome::Changed);
    kwin::tests::push_snapshot(&k, snapshot_desktop_one_with_no_windows());
    assert_eq!(k.read(), ReadOutcome::Changed);
    assert_eq!(k.state.windows.len(), 0);
    assert_eq!(k.state.workspaces.len(), 1);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib kwin::tests`
Expected: FAIL — no `push_snapshot`, no snapshot handling.

- [ ] **Step 3: Implement the service and the fold**

Register the name on the `Bus` thread; parse the snapshot with `serde_json` (already a dependency) into a `Snapshot` struct in `src/kwin.rs`; map it into `state` (`addr` = the window `id`; `workspace` = `desktop`). A malformed snapshot is logged and ignored, leaving the previous state — never a panic.

- [ ] **Step 4: Extend the KWin script to the final snapshot shape**

Update `dist/kwin-script/contents/code/main.js` from Task 1's probe shape to the exact JSON above.

- [ ] **Step 5: Run them to verify they pass**

Run: `cargo test --lib kwin::tests -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/kwin.rs dist/kwin-script/contents/code/main.js
git commit -m "kwin: take the window list and focus from the KWin script"
```

---

### Task 5: Point the previews and the preview state at the seam

The `--png` preview path and the scene tests use `fake_hypr: Option<HyprState>` and `hypr_state()`. Re-point them so the KDE path is previewable without a desktop, which is how most later work will be checked.

**Files:**
- Modify: `src/main.rs` (`fake_hypr`, `hypr_state`, `expand_preview`, `level_preview`, `app_focus`, the `--png` block)

**Interfaces:**
- Consumes: `Kwin` (Task 4).
- Produces: `App::desktop_state(&self) -> Option<&hypr::HyprState>` (the fake when set, else the live state when connected); `--png --level`, `--expand` and `--focus-class` keep their exact meanings.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn png_previews_draw_workspaces_without_a_desktop() {
    // --level 1 with a fake state draws the strip with no session at all.
    let out = crate::scenes::tests::ws_scene(&font(), toml, Some(&ws_state()));
    assert!(out.contains("kitty")); // the strip's window icon, by class
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib png_previews_draw_workspaces_without_a_desktop`
Expected: FAIL (or the test does not exist yet and must be written against the renamed accessor).

- [ ] **Step 3: Re-point the accessors**

Rename `hypr_state` to `desktop_state` and `fake_hypr` to `fake_desktop`; update the `--png` block (`main.rs:2106-2171`) and the test at `main.rs:4352` to set `fake_desktop`. Keep the flag names and semantics unchanged.

- [ ] **Step 4: Run the suite**

Run: `cargo test` and `cargo build && ./target/debug/barux bar --png /tmp/bar.png --config config.example.toml --level 1`
Expected: PASS, and the PNG shows the capsules and the strip.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "kwin: preview the desktop items without a running desktop"
```

---

### Task 6: The strip's per-window data and the per-app buttons

Make the window icons, their per-desktop attribution, and the `app_actions` rows work from KWin data. The state machine is unchanged; what changes is the class/identity that reaches it.

**Files:**
- Modify: `src/kwin.rs`
- Modify: `src/config.rs` (the `focus_window` argument validator for the KDE action type only — see Task 9; here, only the window-identity validator used by the strip)
- Test: `src/kwin.rs`, `src/scenes.rs`

**Interfaces:**
- Consumes: `Kwin::state` (Task 4).
- Produces: `pub fn window_arg(id: &str) -> Result<String>` in `src/kwin.rs`, accepting the KWin window id form recorded in `docs/kwin-notes.md`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn two_windows_of_a_class_land_on_their_own_desktops() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    kwin::tests::push_snapshot(&k, snapshot_two_desktops_two_kitty());
    let view = workspaces::WsView::from_state(&k.state, config::WsShow::Existing);
    assert_eq!(view.workspaces.iter().map(|w| w.windows.len()).collect::<Vec<_>>(), vec![1, 1]);
}

#[test]
fn an_empty_class_gets_a_letter_circle_not_a_blank_cell() {
    // The strip resolves an empty class to WinGlyph::Letter, never to nothing.
    let scene = crate::scenes::tests::ws_scene(&font(), toml, Some(&state_with_empty_class()));
    assert!(scene.contains("?") || scene.contains("letter")); // the letter glyph path was taken
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib`
Expected: FAIL.

- [ ] **Step 3: Implement `window_arg` and confirm the projection**

Confirm by test that `WsView::from_state` attributes windows by `workspace` and that the letter-circle fallback in `workspaces.rs` (the `WinGlyph::Letter` path) is reached for an empty class. If `resourceClass` proves empty in practice for some apps (Task 1's notes), fall back to `desktopFileName` in the *script*, so `state` still carries a usable class.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test --lib -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/kwin.rs src/scenes.rs
git commit -m "kwin: per-desktop window icons and classes in the strip"
```

---

### Task 7: The desktop actions — switch, focus, overview

Make the touches act: a capsule switches desktop, a window icon focuses it (and unfolds its buttons), the `overview_action` drag shows the Overview.

**Files:**
- Modify: `src/kwin.rs`
- Modify: `src/main.rs` (the `UiEvent::Workspace` / `FocusWindow` / `Overview` arms)
- Test: `src/kwin.rs`

**Interfaces:**
- Consumes: `Bus` (Task 2), the desktop interface (Task 3), `window_arg` (Task 6).
- Produces: `Kwin::dispatch_workspace(&self, id: &str) -> Result<()>` (writes `current` on `/VirtualDesktopManager`), `Kwin::dispatch_focus_window(&self, addr: &str) -> Result<()>` (loads a generated one-shot KWin script via `org.kde.kwin.Scripting` `loadScript` + `run` + `unloadScript`), `Kwin::overview(&self) -> Result<()>` (`org.kde.kglobalaccel` `Component.invokeShortcut "Overview"`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn tapping_a_capsule_writes_the_current_desktop() {
    let mut k = kwin::tests::kwin_on(fake_bus_with(&[(1, "Uno"), (2, "Dos")], 1));
    k.dispatch_workspace("2").unwrap();
    assert_eq!(kwin::tests::current_desktop(&k), 2);
}

#[test]
fn tapping_a_window_icon_loads_a_script_that_focuses_it() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    k.dispatch_focus_window("a2").unwrap();
    assert!(kwin::tests::loaded_scripts(&k).iter().any(|s| s.contains("a2")));
}

#[test]
fn the_overview_drag_invokes_the_overview_shortcut() {
    let mut k = kwin::tests::kwin_on(fake_bus());
    k.overview().unwrap();
    assert_eq!(kwin::tests::invoked_shortcuts(&k), vec!["Overview"]);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib kwin::tests`
Expected: FAIL.

- [ ] **Step 3: Implement the three dispatches**

The generated script is written to the user's runtime dir (not `/tmp`), loaded, run and unloaded; if any step fails the action is logged and ignored, and a later tap may retry. When the desktop is not connected, each dispatch logs and returns `Err` exactly as the Hyprland path does today.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test --lib kwin::tests -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/kwin.rs src/main.rs
git commit -m "kwin: switch desktops, focus windows and show the overview"
```

---

### Task 8: `app_keys` follows the KWin focus

The `app_keys` slot is told the focused class and title by `Scene::set_app_focus`; it must now be told KWin's. This should be a wiring task, not new logic.

**Files:**
- Modify: `src/main.rs` (`App::app_focus`, `sync_app_keys`)
- Test: `src/main.rs`, `src/scenes.rs`

**Interfaces:**
- Consumes: `Kwin::state.focused_window()` (Task 4); `AppKeys::set_focus` (existing).
- Produces: `App::app_focus(&self) -> (String, String)` returning the focused window's class and title, or `("", "")` when no desktop is connected.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn app_keys_shows_the_kwin_focused_windows_set() {
    // With a fake desktop whose focus is a zen window, the zen set is the active one.
    let scene = crate::scenes::tests::ws_scene(&font(), app_keys_toml, Some(&state_focused_on("zen")));
    assert!(scene.contains("zen"));
}

#[test]
fn without_a_desktop_app_keys_shows_its_default_set() {
    let scene = crate::scenes::tests::ws_scene(&font(), app_keys_toml, None);
    assert!(scene.contains("default")); // default_set, not a panic
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib`
Expected: FAIL.

- [ ] **Step 3: Re-point the focus accessor**

Point `app_focus` at the live state via `desktop_state()`; keep `--focus-class` as the preview override.

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test --lib -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/scenes.rs
git commit -m "kwin: app_keys follows the KWin focus"
```

---

### Task 9: The Plasma session environment and the `kwin` action

Give launched commands a Plasma session, and give the configuration a first-class KDE action to replace `hyprctl`.

**Files:**
- Modify: `src/user.rs` (the desktop block in `session_env_in`, and the `HyprEnv` → `DesktopEnv` rename)
- Modify: `src/kwin.rs` (`session_env` returns the renamed type)
- Modify: `src/config.rs` (`Action`, the new payload struct, `validate_action`)
- Modify: `src/main.rs` (`run_action`, the action dispatch, `Runner::set_hypr`/`has_hypr` call sites)
- Modify: `src/runner.rs` (`HyprEnv` field and `set_hypr`/`has_hypr` renamed; `Purpose`, if the action needs a process)
- Test: `src/user.rs`, `src/config.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `user::HyprEnv` is renamed `user::DesktopEnv` (it keeps `wayland_display` and drops the Hyprland `signature`); `Runner::set_hypr`/`has_hypr` become `set_desktop`/`has_desktop`; `Kwin::session_env` (Task 2, where it still returns `HyprEnv`) returns `Option<DesktopEnv>`. Every call site the interface map lists is updated. `session_env` sets `XDG_SESSION_TYPE=wayland` and `XDG_CURRENT_DESKTOP=KDE` when a Plasma session is detected.
  - In `config.rs`: `Action::Kwin(KwinAction)` with `pub struct KwinAction { pub desktop: Option<u32>, pub focus_window: Option<String>, pub exec: Option<Vec<String>>, pub overview: Option<bool> }`, exactly one set, validated like `HyprctlAction::to_action`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_plasma_session_gets_kde_variables() {
    let env = user::session_env(&user, Some(&DesktopEnv { wayland_display: Some("wayland-0".into()) }));
    assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), Some("KDE"));
    assert_eq!(get(&env, "XDG_SESSION_TYPE"), Some("wayland"));
}

#[test]
fn a_kwin_action_needs_exactly_one_field() {
    assert!(kwin_action("desktop = 2").is_ok());
    assert!(kwin_action("").is_err());
    assert!(kwin_action("desktop = 2\nexec = [\"kitty\"]").is_err());
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --lib`
Expected: FAIL.

- [ ] **Step 3: Implement the environment and the action**

The environment keeps the existing D-Bus and Wayland-socket discovery; only the Hyprland-specific tail is replaced. `run_action` gains an arm mirroring `Action::Hyprctl`: switch desktop writes `current`, focus loads a generated script, exec launches as the session user, overview invokes the shortcut.

- [ ] **Step 4: Run the suite**

Run: `cargo test` and `cargo clippy --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/user.rs src/config.rs src/main.rs src/runner.rs
git commit -m "kwin: a Plasma session environment and a kwin action"
```

---

### Task 10: Install the KWin script

Make `install.sh` put the script in place and enable it, and `uninstall.sh` take it away, so no one hand-edits `kwinrc`.

**Files:**
- Modify: `install.sh`
- Modify: `uninstall.sh`
- Modify: `dist/kwin-script/metadata.json` (final id, name, version)
- Test: `tests/install_plan.rs` (a new shell-out test in the style of the existing `tests/`)

**Interfaces:**
- Consumes: the script package (Tasks 1, 4).
- Produces: installed at `~/.local/share/kwin/scripts/<id>/`, enabled by `kwinrc` `[Plugins]` `<id>Enabled=true`, followed by `org.kde.KWin` `reconfigure`; removed by `uninstall.sh`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_install_plan_includes_the_kwin_script() {
    let out = run_install("--check");
    assert!(out.contains("kwin/scripts/"));
    assert!(out.contains("Enabled=true"));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test install_plan`
Expected: FAIL.

- [ ] **Step 3: Add the install and uninstall steps**

`install.sh` refuses to run as root already; add the per-user install and the `kwinrc` edit (via `kwriteconfig6` if present, else a careful append) and the `reconfigure` call, next to the existing installs at `install.sh:322-326`, and the matching removal in `uninstall.sh`'s `FILES+=`/step near line 82.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --test install_plan` and `./install.sh --check`
Expected: PASS; the plan lists the script and the enable step.

- [ ] **Step 5: Commit**

```bash
git add install.sh uninstall.sh dist/kwin-script tests/install_plan.rs
git commit -m "kwin: install and remove the KWin script"
```

---

### Task 11: The example configuration and the guide

The last task: the KDE-facing configuration surface, and documentation that lets someone else run this.

**Files:**
- Modify: `config.example.toml` (drop the virtual `esc`, KDE recipes)
- Modify: `docs/GUIDE.md`
- Test: `tests/config_example.rs` or the existing example-loading test

**Interfaces:**
- Consumes: everything above.
- Produces: an example that loads on a KDE machine and demonstrates the `kwin` action and the desktop items.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_example_config_has_no_virtual_esc() {
    let cfg = load_example();
    assert!(!cfg.layers[0].items.iter().any(|i| i.id == "esc"));
}

#[test]
fn the_example_config_uses_the_kwin_action() {
    assert!(std::fs::read_to_string("config.example.toml").unwrap().contains("type = \"kwin\""));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --test config_example`
Expected: FAIL.

- [ ] **Step 3: Update the example and the guide**

Remove the `esc` button; move the `hyprctl` recipe to `kwin`; add a KDE section to the guide covering the desktop items, the KWin script (what it is, that install.sh enables it, how to disable it), how to read a window's class on KDE, and how to go back to tiny-dfr.

- [ ] **Step 4: Run the suite**

Run: `cargo test` and `cargo build && ./target/debug/barux bar --png /tmp/bar.png --config config.example.toml`
Expected: PASS; the PNG renders.

- [ ] **Step 5: Commit**

```bash
git add config.example.toml docs/GUIDE.md tests/config_example.rs
git commit -m "kwin: KDE example configuration and guide"
```
