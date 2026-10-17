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
use std::os::fd::BorrowedFd;

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
}

impl Kwin {
    pub fn new(preferred_uid: Option<u32>) -> Kwin {
        Kwin {
            preferred_uid,
            bus: None,
            provider: None,
            state: DesktopState::default(),
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
        self.bus = Some(bus);
        self.state = DesktopState::default();
        // Task 3 subscribes to the virtual-desktop signals; Task 4 serves the
        // KWin script's name.
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
        let _notices = bus.take();
        // Task 3 and Task 4 fold the notices into `state`.
        ReadOutcome::Unchanged
    }

    pub fn disconnect(&mut self) {
        self.bus = None;
        self.state = DesktopState::default();
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

    #[test]
    fn without_a_desktop_workspaces_take_no_room() {
        // A Kwin that never connected reports no state, so the item is filtered out.
        let k = Kwin::new(None);
        assert!(!k.connected());
        assert!(k.state.workspaces.is_empty());
    }
}
