//! KWin/Plasma desktop source: virtual desktops from D-Bus
//! (`org.kde.KWin.VirtualDesktopManager`) and the window list from a KWin script
//! that pushes snapshots to barux's own session-bus name. Barux may run as root,
//! so every connection is made to the *session user's* bus.
//!
//! The neutral desktop state stays `crate::hypr::HyprState`, imported here under a
//! neutral name, so `WsView::from_state`, the `workspaces` state machine,
//! `appkeys` and `scenes` are untouched by the move off Hyprland.

use crate::dbus::{Bus, Notice, Notifier};
use crate::hypr::{HyprState as DesktopState, ReadOutcome, Window};
use crate::hyprctl::ConfigProvider;
use crate::user::{DesktopEnv, SessionUser};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use zbus::blocking::Proxy;
use zbus::zvariant::Value;

/// KWin's virtual-desktop interface (Plasma 6). See `docs/kwin-notes.md`: the exact
/// property signatures are UNSURE until `tools/kwin-probe.sh` is run on a session.
const DEST: &str = "org.kde.KWin";
const VDM_PATH: &str = "/VirtualDesktopManager";
const VDM_IFACE: &str = "org.kde.KWin.VirtualDesktopManager";
/// A loaded script's own object: `run` lives here, not on `/Scripting`.
const SCRIPT_PATH_PREFIX: &str = "/Scripting/Script";
const SCRIPT_IFACE: &str = "org.kde.kwin.Script";
/// Any of these means the desktop list may have changed: re-read it (event-driven;
/// no timer). `currentChanged` is the switch; the rest are add/remove/rename.
const VDM_SIGNALS: [&str; 4] = [
    "currentChanged",
    "desktopCreated",
    "desktopRemoved",
    "desktopDataChanged",
];
/// What barux serves the KWin script on: the script calls `Snapshot(json)` here.
const BARUX_NAME: &str = "org.barux.KWin";
const BARUX_PATH: &str = "/KWin";
const BARUX_IFACE: &str = "org.barux.KWin";
/// KWin's scripting interface: load a generated one-shot script, run it, unload it.
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING_IFACE: &str = "org.kde.kwin.Scripting";
/// The global-shortcut service: invoke KWin's "Overview".
const KGLOBALACCEL: &str = "org.kde.kglobalaccel";
const KGLOBALACCEL_PATH: &str = "/component/kwin";
const KGLOBALACCEL_IFACE: &str = "org.kde.kglobalaccel.Component";

/// A KWin window id as the bar sends it: the KWin script's `internalId`, a
/// UUID-like string (`docs/kwin-notes.md` §2). Kept to `[A-Za-z0-9._-]`, ≤ 64
/// bytes: the id also names the generated focus script (a file the daemon writes
/// and a KWin plugin name), so anything else is refused.
pub fn window_arg(id: &str) -> Result<String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !ok {
        bail!("bad window id {id:?}");
    }
    Ok(id.to_string())
}

/// The one-shot KWin script that focuses the window with `addr` (its `internalId`):
/// generated, then loaded, run and unloaded at once.
fn focus_script(addr: &str) -> String {
    format!(
        "// barux: focus one window (generated; loaded, run and unloaded at once).\n\
         const id = {addr:?};\n\
         const w = workspace.windowList().find(function (w) {{ return String(w.internalId) === id; }});\n\
         if (w) {{\n    workspace.activeWindow = w;\n}}\n"
    )
}

/// The live KWin desktop source. While it is not connected, `state` is empty, so
/// every desktop-aware item takes no room.
pub struct Kwin {
    preferred_uid: Option<u32>,
    bus: Option<Bus>,
    /// KWin has no Hyprland config-provider syntax, so this stays `None`. It is
    /// kept so the Hyprland action's dispatch site in `main.rs` is unchanged
    /// (that action is off the live path on this branch; Task 9 adds the `kwin`
    /// action in its place).
    pub provider: Option<ConfigProvider>,
    pub state: DesktopState,
    /// `state` as last reported to the loop, for `read`'s Changed/Unchanged.
    announced: DesktopState,
    /// Whether a KWin script snapshot has been seen: once it has, it is the live
    /// source for the desktop list too (the D-Bus read is the pre-script fallback).
    have_snapshot: bool,
    /// Where generated one-shot scripts are written (the session user's runtime
    /// directory; tests override it).
    script_dir: Option<PathBuf>,
}

impl Kwin {
    pub fn new(preferred_uid: Option<u32>) -> Kwin {
        Kwin {
            preferred_uid,
            bus: None,
            provider: None,
            state: DesktopState::default(),
            announced: DesktopState::default(),
            have_snapshot: false,
            script_dir: None,
        }
    }

    /// The bus wake pipe, while connected (for the epoll loop).
    pub fn event_fd(&self) -> Option<BorrowedFd<'_>> {
        self.bus.as_ref().map(Bus::fd)
    }

    /// Connects to the session user's bus. KWin itself is reached through it; a
    /// bus that answers before KWin does still connects (the state stays empty).
    pub fn connect(&mut self) -> Result<()> {
        let env = self.session_env_vec()?;
        let bus = Bus::connect(&env)?;
        self.attach(bus)
    }

    /// Subscribes to the virtual-desktop signals, serves the KWin script's name and
    /// reads the initial list.
    fn attach(&mut self, bus: Bus) -> Result<()> {
        for signal in VDM_SIGNALS {
            bus.watch(DEST, VDM_PATH, VDM_IFACE, signal)?;
        }
        // The KWin script reports here; barux owns the name (never the desktop's).
        bus.serve(
            BARUX_NAME,
            BARUX_PATH,
            SnapshotIface {
                notify: bus.notifier(),
            },
        )?;
        self.bus = Some(bus);
        self.provider = None;
        self.state = DesktopState::default();
        self.announced = DesktopState::default();
        self.have_snapshot = false;
        // KWin may not be up yet, or the script not loaded: tolerate it and wait.
        if let Err(e) = self.read_desktops() {
            eprintln!("desktop: {e:#}; waiting for KWin");
        }
        Ok(())
    }

    /// Whether the session bus is connected (the state is live).
    pub fn connected(&self) -> bool {
        self.bus.is_some()
    }

    /// Reads what the bus received and folds it into `state`.
    pub fn read(&mut self) -> ReadOutcome {
        let notices = {
            let Some(bus) = self.bus.as_mut() else {
                return ReadOutcome::Disconnected;
            };
            bus.drain();
            bus.take()
        };
        for notice in notices {
            if let Notice::Snapshot(json) = notice {
                self.have_snapshot = true;
                self.apply_snapshot(&json);
            }
        }
        if !self.have_snapshot {
            // Until the KWin script reports, the desktop list comes from D-Bus.
            if let Err(e) = self.read_desktops() {
                eprintln!("desktop: {e:#}");
            }
        }
        if self.state != self.announced {
            self.announced = self.state.clone();
            ReadOutcome::Changed
        } else {
            ReadOutcome::Unchanged
        }
    }

    /// Folds a KWin script snapshot into `state`; a malformed one is logged and
    /// ignored, leaving the previous state (never a panic).
    fn apply_snapshot(&mut self, json: &str) {
        let snap: Snapshot = match serde_json::from_str(json) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("desktop: bad snapshot ignored: {e:#}");
                return;
            }
        };
        self.state.workspaces = snap
            .desktops
            .iter()
            .map(|d| (d.position, d.name.clone()))
            .collect();
        self.state.active_workspace = (snap.current > 0).then_some(snap.current);
        self.state.windows = snap
            .windows
            .iter()
            .map(|w| Window {
                addr: w.id.clone(),
                class: w.class.clone(),
                title: w.title.clone(),
                workspace: w.desktop,
            })
            .collect();
        self.state.focused = snap.focused;
    }

    /// Re-reads the virtual desktops and the current one from
    /// `org.kde.KWin.VirtualDesktopManager`. `desktops` is `a(iss)` — each entry is
    /// (position, id, name) — and `current` is the current desktop's id. Returns
    /// whether `state` changed.
    fn read_desktops(&mut self) -> Result<bool> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow!("desktop not connected"))?;
        let vdm = Proxy::new(bus.connection(), DEST, VDM_PATH, VDM_IFACE)?;
        let desktops: Vec<(i32, String, String)> = vdm.get_property("desktops")?;
        let current: String = vdm.get_property("current")?;
        let mut workspaces = BTreeMap::new();
        let mut active = None;
        for (position, id, name) in desktops {
            let position = position as i64;
            workspaces.insert(position, name);
            if id == current {
                active = Some(position);
            }
        }
        let changed =
            self.state.workspaces != workspaces || self.state.active_workspace != active;
        self.state.workspaces = workspaces;
        self.state.active_workspace = active;
        Ok(changed)
    }

    pub fn disconnect(&mut self) {
        self.bus = None;
        self.provider = None;
        self.state = DesktopState::default();
        self.announced = DesktopState::default();
        self.have_snapshot = false;
    }

    /// Switches to the virtual desktop at the bar's position `id` (1-based, as a
    /// string): writes `current` on `/VirtualDesktopManager` to that desktop's id.
    pub fn dispatch_workspace(&self, id: &str) -> Result<()> {
        let position: i64 = id
            .trim()
            .parse()
            .ok()
            .filter(|p| *p >= 1)
            .ok_or_else(|| anyhow!("bad workspace id {id:?}"))?;
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow!("desktop not connected"))?;
        let vdm = Proxy::new(bus.connection(), DEST, VDM_PATH, VDM_IFACE)?;
        let desktops: Vec<(i32, String, String)> = vdm.get_property("desktops")?;
        let target = desktops
            .into_iter()
            .find(|(p, _, _)| *p as i64 == position)
            .ok_or_else(|| anyhow!("no virtual desktop at position {position}"))?;
        vdm.set_property("current", target.1)?;
        Ok(())
    }

    /// Focuses the window with `addr` (the KWin script's `internalId`): writes a
    /// one-shot KWin script to the runtime dir and has KWin load, run and unload it.
    pub fn dispatch_focus_window(&self, addr: &str) -> Result<()> {
        let addr = window_arg(addr)?;
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow!("desktop not connected"))?;
        let dir = self
            .script_dir
            .clone()
            .or_else(|| self.session_runtime_dir())
            .ok_or_else(|| anyhow!("no runtime directory to write the focus script into"))?;
        let path = dir.join(format!("barux-focus-{addr}.js"));
        std::fs::write(&path, focus_script(&addr))
            .with_context(|| format!("writing {}", path.display()))?;
        let path = path.to_string_lossy().to_string();
        // `loadScript(filePath, pluginName)` returns the script's id (or -1 if that
        // plugin name is already loaded); `run` lives on the script's own object.
        // A unique plugin name per tap keeps this from clashing with a script that
        // failed to unload.
        let plugin = format!("barux-focus-{addr}");
        let id: i32 = Proxy::new(bus.connection(), DEST, SCRIPTING_PATH, SCRIPTING_IFACE)?
            .call("loadScript", &(path.as_str(), plugin.as_str()))?;
        let script_path = format!("{SCRIPT_PATH_PREFIX}{id}");
        if let Err(e) = Proxy::new(bus.connection(), DEST, script_path.as_str(), SCRIPT_IFACE)
            .and_then(|p| p.call::<_, _, ()>("run", &()))
        {
            eprintln!("desktop: running the focus script: {e:#}");
        }
        if let Err(e) = bus.call(
            DEST,
            SCRIPTING_PATH,
            SCRIPTING_IFACE,
            "unloadScript",
            &[Value::from(plugin.as_str())],
        ) {
            eprintln!("desktop: unloading the focus script: {e:#}");
        }
        Ok(())
    }

    /// Shows/hides KWin's Overview: invokes its `Overview` global shortcut.
    pub fn overview(&self) -> Result<()> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow!("desktop not connected"))?;
        bus.call(
            KGLOBALACCEL,
            KGLOBALACCEL_PATH,
            KGLOBALACCEL_IFACE,
            "invokeShortcut",
            &[Value::from("Overview")],
        )?;
        Ok(())
    }

    /// The session user's runtime directory (where generated scripts go).
    fn session_runtime_dir(&self) -> Option<PathBuf> {
        let uid = self.preferred_uid?;
        SessionUser::from_uid(uid).ok().map(|u| u.runtime_dir())
    }

    /// What commands need to talk to this desktop: the Wayland socket.
    pub fn session_env(&self) -> Option<DesktopEnv> {
        self.bus.as_ref()?;
        let env = self.session_env_vec().ok()?;
        let wayland_display = env
            .iter()
            .find(|(k, _)| k == "WAYLAND_DISPLAY")
            .map(|(_, v)| v.clone());
        Some(DesktopEnv { wayland_display })
    }

    /// The session user's environment (the bus address among it).
    fn session_env_vec(&self) -> Result<Vec<(String, String)>> {
        let uid = self
            .preferred_uid
            .ok_or_else(|| anyhow!("no session user"))?;
        let user = SessionUser::from_uid(uid)?;
        Ok(crate::user::session_env(&user, None))
    }
}

/// The object barux serves the KWin script on (`org.barux.KWin` `/KWin`): the
/// script calls `Snapshot(json)` with a JSON snapshot of the desktops and windows.
struct SnapshotIface {
    notify: Notifier,
}

#[zbus::interface(name = "org.barux.KWin")]
impl SnapshotIface {
    /// A JSON snapshot of KWin's desktops and windows (see `dist/kwin-script/`).
    fn snapshot(&self, json: String) {
        self.notify.push(Notice::Snapshot(json));
    }
}

/// One KWin script snapshot. The wire shape is fixed in `docs/kwin-notes.md` and
/// produced by `dist/kwin-script/contents/code/main.js`.
#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    desktops: Vec<SnapshotDesktop>,
    #[serde(default)]
    current: i64,
    #[serde(default)]
    windows: Vec<SnapshotWindow>,
    #[serde(default)]
    focused: Option<String>,
}

#[derive(Deserialize)]
struct SnapshotDesktop {
    position: i64,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct SnapshotWindow {
    id: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    desktop: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    /// A private session bus: a real `dbus-daemon`, killed on drop. `None` when the
    /// daemon isn't on `PATH` (the tests then skip, like the font-dependent ones).
    struct Daemon {
        address: String,
        pid: i32,
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }

    fn daemon() -> Option<Daemon> {
        let out = Command::new("dbus-daemon")
            .args(["--session", "--fork", "--print-address=1", "--print-pid=1"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let address = lines.next()?.trim().to_string();
        let pid = lines.next()?.trim().parse().ok()?;
        Some(Daemon { address, pid })
    }

    fn env(address: &str) -> Vec<(String, String)> {
        vec![("DBUS_SESSION_BUS_ADDRESS".to_string(), address.to_string())]
    }

    /// A fake KWin VDM: desktops by position, the current position, real D-Bus.
    #[derive(Default)]
    struct VdmState {
        desktops: Vec<(i64, String)>,
        current: i64,
    }

    impl VdmState {
        fn current_id(&self) -> String {
            self.current.to_string()
        }
    }

    struct VdmIface {
        state: Arc<Mutex<VdmState>>,
    }

    #[zbus::interface(name = "org.kde.KWin.VirtualDesktopManager")]
    impl VdmIface {
        #[zbus(property, name = "desktops")]
        fn desktops(&self) -> Vec<(i32, String, String)> {
            self.state
                .lock()
                .unwrap()
                .desktops
                .iter()
                .map(|(p, n)| (*p as i32, p.to_string(), n.clone()))
                .collect()
        }

        #[zbus(property, name = "current")]
        fn current(&self) -> String {
            self.state.lock().unwrap().current_id()
        }

        #[zbus(property, name = "current")]
        fn set_current(&self, value: String) {
            if let Ok(position) = value.parse::<i64>() {
                self.state.lock().unwrap().current = position;
            }
        }
    }

    /// The fake KWin scripting interface: records each loaded script's text. `run`
    /// lives on the script object (`ScriptIface`), as in KWin.
    struct ScriptingIface {
        scripts: Arc<Mutex<Vec<String>>>,
    }

    #[zbus::interface(name = "org.kde.kwin.Scripting")]
    impl ScriptingIface {
        #[zbus(name = "loadScript")]
        fn load_script(&self, path: String, _plugin: String) -> i32 {
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            let mut scripts = self.scripts.lock().unwrap();
            scripts.push(body);
            1
        }

        #[zbus(name = "unloadScript")]
        fn unload_script(&self, _plugin: String) {}
    }

    /// A loaded script's own object, where `run` lives (`/Scripting/Script<i>`).
    struct ScriptIface;

    #[zbus::interface(name = "org.kde.kwin.Script")]
    impl ScriptIface {
        #[zbus(name = "run")]
        fn run(&self) {}
    }

    /// The fake global-shortcut component: records the shortcuts invoked.
    struct ComponentIface {
        shortcuts: Arc<Mutex<Vec<String>>>,
    }

    #[zbus::interface(name = "org.kde.kglobalaccel.Component")]
    impl ComponentIface {
        #[zbus(name = "invokeShortcut")]
        fn invoke_shortcut(&self, name: String) {
            self.shortcuts.lock().unwrap().push(name);
        }
    }

    struct Fake {
        _daemon: Daemon,
        server: Bus,
        state: Arc<Mutex<VdmState>>,
        scripts: Arc<Mutex<Vec<String>>>,
        shortcuts: Arc<Mutex<Vec<String>>>,
        dir: crate::testdir::TestDir,
        address: String,
    }

    impl Fake {
        /// A fresh connection to the same private bus, for the Kwin under test.
        fn bus(&self) -> Bus {
            Bus::connect(&env(&self.address)).unwrap()
        }

        /// Where generated focus scripts are written.
        fn script_dir(&self) -> PathBuf {
            self.dir.to_path_buf()
        }

        /// The current virtual desktop position (as the VDM reports it).
        fn current_position(&self) -> i64 {
            self.state.lock().unwrap().current
        }

        /// The text of each script KWin was asked to load.
        fn loaded_scripts(&self) -> Vec<String> {
            self.scripts.lock().unwrap().clone()
        }

        /// The global shortcuts invoked.
        fn invoked_shortcuts(&self) -> Vec<String> {
            self.shortcuts.lock().unwrap().clone()
        }

        fn emit(&self, signal: &str) {
            let current = self.state.lock().unwrap().current_id();
            let _ = self.server.connection().emit_signal(
                None::<&str>,
                VDM_PATH,
                VDM_IFACE,
                signal,
                &(current.as_str(),),
            );
        }

        fn switch_to(&self, position: i64) {
            self.state.lock().unwrap().current = position;
            self.emit("currentChanged");
        }

        fn set_desktops(&self, desktops: &[(i64, &str)], current: i64) {
            {
                let mut s = self.state.lock().unwrap();
                s.desktops = desktops.iter().map(|(p, n)| (*p, n.to_string())).collect();
                s.current = current;
            }
            self.emit("desktopDataChanged");
        }
    }

    /// Serves the action side of the fake: KWin's scripting interface and the
    /// global-shortcut component (both under `org.kde.KWin`'s scripting object and
    /// the separate `org.kde.kglobalaccel` name).
    fn serve_actions(
        server: &Bus,
        scripts: Arc<Mutex<Vec<String>>>,
        shortcuts: Arc<Mutex<Vec<String>>>,
    ) {
        server
            .connection()
            .object_server()
            .at(SCRIPTING_PATH, ScriptingIface { scripts })
            .unwrap();
        // The script object `loadScript` returns (id 1 in this fake), where `run` is.
        server
            .connection()
            .object_server()
            .at("/Scripting/Script1", ScriptIface)
            .unwrap();
        server.connection().request_name(KGLOBALACCEL).unwrap();
        server
            .connection()
            .object_server()
            .at(KGLOBALACCEL_PATH, ComponentIface { shortcuts })
            .unwrap();
    }

    /// A fake VDM on a private bus, with `desktops` (position, name) and `current`.
    fn fake_bus_with(desktops: &[(i64, &str)], current: i64) -> Fake {
        let d = daemon().expect("dbus-daemon on PATH");
        let address = d.address.clone();
        let state = Arc::new(Mutex::new(VdmState {
            desktops: desktops.iter().map(|(p, n)| (*p, n.to_string())).collect(),
            current,
        }));
        let scripts = Arc::new(Mutex::new(Vec::new()));
        let shortcuts = Arc::new(Mutex::new(Vec::new()));
        let server = Bus::connect(&env(&address)).unwrap();
        server
            .serve(
                DEST,
                VDM_PATH,
                VdmIface {
                    state: Arc::clone(&state),
                },
            )
            .unwrap();
        serve_actions(&server, Arc::clone(&scripts), Arc::clone(&shortcuts));
        Fake {
            _daemon: d,
            server,
            state,
            scripts,
            shortcuts,
            dir: crate::testdir::new("kwin-fake"),
            address,
        }
    }

    /// A bare private bus with no running KWin (the desktop interface answers only
    /// for the scripting/global-shortcut side).
    fn fake_bus() -> Fake {
        let d = daemon().expect("dbus-daemon on PATH");
        let address = d.address.clone();
        let state = Arc::new(Mutex::new(VdmState::default()));
        let scripts = Arc::new(Mutex::new(Vec::new()));
        let shortcuts = Arc::new(Mutex::new(Vec::new()));
        let server = Bus::connect(&env(&address)).unwrap();
        server.connection().request_name(DEST).unwrap();
        serve_actions(&server, Arc::clone(&scripts), Arc::clone(&shortcuts));
        Fake {
            _daemon: d,
            server,
            state,
            scripts,
            shortcuts,
            dir: crate::testdir::new("kwin-fake"),
            address,
        }
    }

    /// Calls barux's served `Snapshot`, as the KWin script would.
    fn push_snapshot(k: &Kwin, json: &str) {
        let conn = k.bus.as_ref().unwrap().connection();
        conn.call_method(
            Some(BARUX_NAME),
            BARUX_PATH,
            Some(BARUX_IFACE),
            "Snapshot",
            &(json,),
        )
        .unwrap();
    }

    fn snapshot_one_window_on_desktop_one() -> &'static str {
        r#"{"desktops":[{"position":1,"name":"Uno"}],"current":1,
            "windows":[{"id":"a1","class":"kitty","title":"htop","desktop":1}],"focused":"a1"}"#
    }

    fn snapshot_desktop_one_with_no_windows() -> &'static str {
        r#"{"desktops":[{"position":1,"name":"Uno"}],"current":1,"windows":[],"focused":null}"#
    }

    /// A Kwin attached to `fake`'s bus (as `connect` would).
    fn kwin_on(fake: &Fake) -> Kwin {
        let mut k = Kwin::new(None);
        k.attach(fake.bus()).unwrap();
        k.script_dir = Some(fake.script_dir());
        k
    }

    #[test]
    fn without_a_desktop_workspaces_take_no_room() {
        // A Kwin that never connected reports no state, so the item is filtered out.
        let k = Kwin::new(None);
        assert!(!k.connected());
        assert!(k.state.workspaces.is_empty());
    }

    #[test]
    fn virtual_desktops_become_workspaces_by_position() {
        let fake = fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2);
        let k = kwin_on(&fake);
        assert_eq!(
            k.state.workspaces,
            BTreeMap::from([(1, "Uno".into()), (2, "Dos".into())])
        );
        assert_eq!(k.state.active_workspace, Some(2));
    }

    #[test]
    fn a_desktop_switch_arrives_without_a_timer() {
        let fake = fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2);
        let mut k = kwin_on(&fake);
        fake.switch_to(1);
        assert!(matches!(k.read(), ReadOutcome::Changed));
        assert_eq!(k.state.active_workspace, Some(1));
    }

    #[test]
    fn a_desktop_appears_and_disappears_between_reads() {
        let fake = fake_bus_with(&[(1, "Uno"), (2, "Dos")], 2);
        let mut k = kwin_on(&fake);
        assert!(matches!(k.read(), ReadOutcome::Changed));
        fake.set_desktops(&[(1, "Uno")], 1);
        assert!(matches!(k.read(), ReadOutcome::Changed));
        assert_eq!(k.state.workspaces.len(), 1);
    }

    #[test]
    fn a_snapshot_becomes_windows_on_their_desktops() {
        let fake = fake_bus();
        let mut k = kwin_on(&fake);
        push_snapshot(
            &k,
            r#"{"desktops":[{"position":1,"name":"Uno"},{"position":2,"name":"Dos"}],
                "current":2,"windows":[{"id":"a1","class":"kitty","title":"htop","desktop":1},
                {"id":"a2","class":"zen","title":"Mail","desktop":2}],"focused":"a2"}"#,
        );
        assert!(matches!(k.read(), ReadOutcome::Changed));
        assert_eq!(k.state.window_count(1), 1);
        assert_eq!(k.state.focused_window().map(|w| w.class.as_str()), Some("zen"));
        assert_eq!(k.state.windows[0].workspace, 1);
    }

    #[test]
    fn a_window_with_no_class_still_arrives() {
        let fake = fake_bus();
        let mut k = kwin_on(&fake);
        push_snapshot(
            &k,
            r#"{"desktops":[{"position":1,"name":"Uno"}],"current":1,
                "windows":[{"id":"a1","class":"","title":"","desktop":1}],"focused":null}"#,
        );
        assert!(matches!(k.read(), ReadOutcome::Changed));
        assert_eq!(k.state.windows[0].class, "");
    }

    #[test]
    fn a_window_that_closes_leaves_its_desktop_in_place() {
        let fake = fake_bus();
        let mut k = kwin_on(&fake);
        push_snapshot(&k, snapshot_one_window_on_desktop_one());
        assert!(matches!(k.read(), ReadOutcome::Changed));
        push_snapshot(&k, snapshot_desktop_one_with_no_windows());
        assert!(matches!(k.read(), ReadOutcome::Changed));
        assert_eq!(k.state.windows.len(), 0);
        assert_eq!(k.state.workspaces.len(), 1);
    }

    fn snapshot_two_desktops_two_kitty() -> &'static str {
        r#"{"desktops":[{"position":1,"name":"Uno"},{"position":2,"name":"Dos"}],"current":1,
            "windows":[{"id":"a1","class":"kitty","title":"one","desktop":1},
            {"id":"a2","class":"kitty","title":"two","desktop":2}],"focused":"a1"}"#
    }

    #[test]
    fn two_windows_of_a_class_land_on_their_own_desktops() {
        let fake = fake_bus();
        let mut k = kwin_on(&fake);
        push_snapshot(&k, snapshot_two_desktops_two_kitty());
        assert!(matches!(k.read(), ReadOutcome::Changed));
        let view = crate::workspaces::WsView::from_state(&k.state, crate::config::WsShow::Existing);
        assert_eq!(
            view.workspaces
                .iter()
                .map(|w| w.windows.len())
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
    }

    #[test]
    fn a_window_id_is_the_scripts_internal_id() {
        assert_eq!(window_arg("a1").unwrap(), "a1");
        assert!(window_arg("60c2a5b0-8f3e-4a1c-9d2e-0f1a2b3c4d5e").is_ok());
        for bad in ["", "two words", "line\nbreak", "/etc/passwd", "a/b", &"x".repeat(65)] {
            assert!(window_arg(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn tapping_a_capsule_writes_the_current_desktop() {
        let fake = fake_bus_with(&[(1, "Uno"), (2, "Dos")], 1);
        let k = kwin_on(&fake);
        k.dispatch_workspace("2").unwrap();
        assert_eq!(fake.current_position(), 2);
    }

    #[test]
    fn tapping_a_window_icon_loads_a_script_that_focuses_it() {
        let fake = fake_bus();
        let k = kwin_on(&fake);
        k.dispatch_focus_window("a2").unwrap();
        assert!(fake.loaded_scripts().iter().any(|s| s.contains("a2")));
    }

    #[test]
    fn the_overview_drag_invokes_the_overview_shortcut() {
        let fake = fake_bus();
        let k = kwin_on(&fake);
        k.overview().unwrap();
        assert_eq!(fake.invoked_shortcuts(), vec!["Overview"]);
    }
}
