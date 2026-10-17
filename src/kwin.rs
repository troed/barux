//! KWin/Plasma desktop source: virtual desktops from D-Bus
//! (`org.kde.KWin.VirtualDesktopManager`) and the window list from a KWin script
//! that pushes snapshots to barux's own session-bus name. Barux may run as root,
//! so every connection is made to the *session user's* bus.
//!
//! The neutral desktop state stays `crate::hypr::HyprState`, imported here under a
//! neutral name, so `WsView::from_state`, the `workspaces` state machine,
//! `appkeys` and `scenes` are untouched by the move off Hyprland.

use crate::dbus::Bus;
use crate::hypr::{HyprState as DesktopState, ReadOutcome};
use crate::hyprctl::ConfigProvider;
use crate::user::{HyprEnv, SessionUser};
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::os::fd::BorrowedFd;
use zbus::blocking::Proxy;

/// KWin's virtual-desktop interface (Plasma 6). See `docs/kwin-notes.md`: the exact
/// property signatures are UNSURE until `tools/kwin-probe.sh` is run on a session.
const DEST: &str = "org.kde.KWin";
const VDM_PATH: &str = "/VirtualDesktopManager";
const VDM_IFACE: &str = "org.kde.KWin.VirtualDesktopManager";
const VDESK_IFACE: &str = "org.kde.KWin.VirtualDesktop";
/// Any of these means the desktop list may have changed: re-read it (event-driven;
/// no timer). `currentChanged` is the switch; the rest are add/remove/rename.
const VDM_SIGNALS: [&str; 4] = [
    "currentChanged",
    "desktopCreated",
    "desktopRemoved",
    "desktopDataChanged",
];

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
}

impl Kwin {
    pub fn new(preferred_uid: Option<u32>) -> Kwin {
        Kwin {
            preferred_uid,
            bus: None,
            provider: None,
            state: DesktopState::default(),
            announced: DesktopState::default(),
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

    /// Subscribes to the virtual-desktop signals and reads the initial list.
    fn attach(&mut self, bus: Bus) -> Result<()> {
        for signal in VDM_SIGNALS {
            bus.watch(DEST, VDM_PATH, VDM_IFACE, signal)?;
        }
        self.bus = Some(bus);
        self.provider = None;
        self.state = DesktopState::default();
        self.announced = DesktopState::default();
        self.read_desktops()?;
        Ok(())
    }

    /// Whether the session bus is connected (the state is live).
    pub fn connected(&self) -> bool {
        self.bus.is_some()
    }

    /// Reads what the bus received and folds it into `state`.
    pub fn read(&mut self) -> ReadOutcome {
        let Some(bus) = self.bus.as_mut() else {
            return ReadOutcome::Disconnected;
        };
        bus.drain();
        let _ = bus.take();
        if let Err(e) = self.read_desktops() {
            eprintln!("desktop: {e:#}");
            return ReadOutcome::Unchanged;
        }
        if self.state.workspaces != self.announced.workspaces
            || self.state.active_workspace != self.announced.active_workspace
        {
            self.announced = self.state.clone();
            ReadOutcome::Changed
        } else {
            ReadOutcome::Unchanged
        }
    }

    /// Re-reads the virtual desktops (position -> name) and the current one from
    /// `org.kde.KWin.VirtualDesktopManager`. Returns whether `state` changed.
    fn read_desktops(&mut self) -> Result<bool> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| anyhow!("desktop not connected"))?;
        let conn = bus.connection();
        let vdm = Proxy::new(conn, DEST, VDM_PATH, VDM_IFACE)?;
        let ids: Vec<String> = vdm.get_property("desktops")?;
        let current: String = vdm.get_property("current")?;
        let mut workspaces = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            // Position is the order; the name is each desktop's own property.
            let name: String = Proxy::new(conn, DEST, format!("{VDM_PATH}/{id}"), VDESK_IFACE)
                .and_then(|d| d.get_property("name"))
                .unwrap_or_default();
            workspaces.insert(i as i64 + 1, name);
        }
        let active = ids
            .iter()
            .position(|id| id == &current)
            .map(|i| i as i64 + 1);
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
    }

    /// Switches to the virtual desktop `id`. Task 7 wires the D-Bus write (and
    /// validates `id`); until then the action fails like a disconnected desktop.
    pub fn dispatch_workspace(&self, _id: &str) -> Result<()> {
        bail!("switching desktops is not implemented yet")
    }

    /// Focuses the window with `addr`. Task 7 loads a generated KWin script.
    pub fn dispatch_focus_window(&self, _addr: &str) -> Result<()> {
        bail!("focusing windows is not implemented yet")
    }

    /// What commands need to talk to this desktop: the Wayland socket. KWin has no
    /// instance signature, so that stays empty (Task 9 drops the field).
    pub fn session_env(&self) -> Option<HyprEnv> {
        self.bus.as_ref()?;
        let env = self.session_env_vec().ok()?;
        let wayland_display = env
            .iter()
            .find(|(k, _)| k == "WAYLAND_DISPLAY")
            .map(|(_, v)| v.clone());
        Some(HyprEnv {
            signature: String::new(),
            wayland_display,
        })
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
        fn ids(&self) -> Vec<String> {
            self.desktops.iter().map(|(p, _)| p.to_string()).collect()
        }
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
        fn desktops(&self) -> Vec<String> {
            self.state.lock().unwrap().ids()
        }

        #[zbus(property, name = "current")]
        fn current(&self) -> String {
            self.state.lock().unwrap().current_id()
        }
    }

    struct DeskIface {
        name: String,
    }

    #[zbus::interface(name = "org.kde.KWin.VirtualDesktop")]
    impl DeskIface {
        #[zbus(property, name = "name")]
        fn name(&self) -> String {
            self.name.clone()
        }
    }

    struct Fake {
        _daemon: Daemon,
        server: Bus,
        state: Arc<Mutex<VdmState>>,
        address: String,
    }

    impl Fake {
        /// A fresh connection to the same private bus, for the Kwin under test.
        fn bus(&self) -> Bus {
            Bus::connect(&env(&self.address)).unwrap()
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

    /// A fake VDM on a private bus, with `desktops` (position, name) and `current`.
    fn fake_bus_with(desktops: &[(i64, &str)], current: i64) -> Fake {
        let d = daemon().expect("dbus-daemon on PATH");
        let address = d.address.clone();
        let state = Arc::new(Mutex::new(VdmState {
            desktops: desktops.iter().map(|(p, n)| (*p, n.to_string())).collect(),
            current,
        }));
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
        for (pos, name) in desktops {
            server
                .connection()
                .object_server()
                .at(
                    format!("{VDM_PATH}/{pos}"),
                    DeskIface {
                        name: name.to_string(),
                    },
                )
                .unwrap();
        }
        Fake {
            _daemon: d,
            server,
            state,
            address,
        }
    }

    /// A Kwin attached to `fake`'s bus (as `connect` would).
    fn kwin_on(fake: &Fake) -> Kwin {
        let mut k = Kwin::new(None);
        k.attach(fake.bus()).unwrap();
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
}
