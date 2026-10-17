//! The unprivileged user that commands run as, and their session environment.

use anyhow::{Context, Result, anyhow};
use nix::unistd::{Gid, Uid, User, getgrouplist};
use std::{
    ffi::CString,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct SessionUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: PathBuf,
    /// Supplementary groups (from NSS, like `id` shows).
    pub groups: Vec<Gid>,
}

impl SessionUser {
    pub fn from_uid(uid: u32) -> Result<SessionUser> {
        let user = User::from_uid(Uid::from_raw(uid))?
            .ok_or(anyhow!("uid {uid} not found in the user database"))?;
        let cname = CString::new(user.name.clone())?;
        let groups = getgrouplist(&cname, user.gid)
            .with_context(|| format!("reading groups of {}", user.name))?;
        Ok(SessionUser {
            uid,
            gid: user.gid.as_raw(),
            name: user.name,
            home: user.dir,
            groups,
        })
    }

    pub fn runtime_dir(&self) -> PathBuf {
        PathBuf::from(format!("/run/user/{}", self.uid))
    }
}

/// Decides who commands run as:
/// 1. `SUDO_UID` (started with sudo);
/// 2. ourselves, if we are not root (then nothing changes identity);
/// 3. `run_as` from the config, only if that file is root-owned and not writable by
///    group/others (otherwise anyone who can edit it could pick the user), and never root;
/// 4. nobody: commands are refused rather than run as root.
pub fn resolve(run_as: Option<&str>, config_path: Option<&Path>) -> Option<SessionUser> {
    let try_uid = |uid: u32| match SessionUser::from_uid(uid) {
        Ok(u) => Some(u),
        Err(e) => {
            eprintln!("user: {e:#}");
            None
        }
    };
    if let Some(uid) = std::env::var("SUDO_UID").ok().and_then(|s| s.parse().ok()) {
        return try_uid(uid);
    }
    let me = Uid::current().as_raw();
    if me != 0 {
        return try_uid(me);
    }
    let name = run_as?;
    let path = config_path?;
    match fs::metadata(path) {
        Ok(m) if m.uid() == 0 && m.permissions().mode() & 0o022 == 0 => {}
        _ => {
            eprintln!(
                "user: ignoring run_as: {} must be owned by root and not group/world-writable",
                path.display()
            );
            return None;
        }
    }
    match User::from_name(name) {
        Ok(Some(u)) if u.uid.as_raw() != 0 => try_uid(u.uid.as_raw()),
        Ok(Some(_)) => {
            eprintln!("user: run_as = root refused");
            None
        }
        _ => {
            eprintln!("user: run_as {name:?} not found");
            None
        }
    }
}

/// The desktop session's details, passed to the commands barux launches.
#[derive(Clone, Default)]
pub struct DesktopEnv {
    /// The compositor's Wayland socket name (a hint, checked like any other).
    pub wayland_display: Option<String>,
}

/// A clean environment for `user`'s session: nothing is inherited from root's (or
/// the service's), and nothing is taken on trust.
///
/// - `XDG_RUNTIME_DIR`: `/run/user/<uid>`, only if it is a real directory owned by
///   the user and private to them (like logind makes it). Without it, none of what
///   follows (all of it lives there).
/// - `WAYLAND_DISPLAY`: the compositor's (from `DesktopEnv`), else the first
///   `wayland-*` there, and only if it is a socket owned by the user.
/// - With a desktop session: `XDG_SESSION_TYPE=wayland` and `XDG_CURRENT_DESKTOP=KDE`.
/// - D-Bus: the user's bus socket, if there is one.
///
/// What is not found is left out: the command runs anyway, and a GUI app fails
/// as it would in a TTY.
pub fn session_env(user: &SessionUser, desktop: Option<&DesktopEnv>) -> Vec<(String, String)> {
    session_env_in(user, desktop, &user.runtime_dir())
}

/// `session_env` with the runtime directory given (a fake one in tests).
fn session_env_in(
    user: &SessionUser,
    desktop: Option<&DesktopEnv>,
    runtime: &Path,
) -> Vec<(String, String)> {
    let home = user.home.display().to_string();
    let mut env = vec![
        ("HOME".into(), home.clone()),
        ("USER".into(), user.name.clone()),
        ("LOGNAME".into(), user.name.clone()),
        (
            "PATH".into(),
            format!("{home}/.local/bin:/usr/local/bin:/usr/bin:/bin"),
        ),
    ];
    // Locale only: the one value worth taking from our own environment.
    for key in ["LANG", "LC_ALL"] {
        if let Ok(v) = std::env::var(key) {
            env.push((key.into(), v));
        }
    }
    if !private_dir(runtime, user.uid) {
        return env;
    }
    env.push(("XDG_RUNTIME_DIR".into(), runtime.display().to_string()));
    // systemd --user's session bus, if the user has one.
    let bus = runtime.join("bus");
    if owned_socket(&bus, user.uid) {
        env.push((
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}", bus.display()),
        ));
    }
    let hint = desktop.and_then(|d| d.wayland_display.as_deref());
    if let Some(w) = wayland_display(runtime, user.uid, hint) {
        env.push(("WAYLAND_DISPLAY".into(), w));
        // A desktop session: the session is Wayland, on Plasma (KWin).
        if desktop.is_some() {
            env.push(("XDG_SESSION_TYPE".into(), "wayland".into()));
            env.push(("XDG_CURRENT_DESKTOP".into(), "KDE".into()));
        }
    }
    env
}

/// The Wayland socket for commands: `hint` if it is good, else the first good
/// `wayland-*` in `runtime` by name. Good: a plain name, a socket, owned by `uid`.
fn wayland_display(runtime: &Path, uid: u32, hint: Option<&str>) -> Option<String> {
    let good = |name: &str| plain_name(name) && owned_socket(&runtime.join(name), uid);
    if let Some(h) = hint.filter(|h| good(h)) {
        return Some(h.to_string());
    }
    let mut names: Vec<String> = fs::read_dir(runtime)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    names.sort();
    names.into_iter().find(|n| good(n))
}

/// A single path component of ordinary characters (no `/`, no `..`).
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A directory (not a symlink to one) owned by `uid`.
fn owned_dir(path: &Path, uid: u32) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.uid() == uid)
}

/// `owned_dir`, and nobody else may enter it.
fn private_dir(path: &Path, uid: u32) -> bool {
    owned_dir(path, uid)
        && fs::symlink_metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 == 0)
}

/// A Unix socket (not a symlink to one) owned by `uid`.
fn owned_socket(path: &Path, uid: u32) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket() && m.uid() == uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, net::UnixListener};

    /// An empty fake runtime directory, private like logind's.
    fn runtime(name: &str) -> crate::testdir::TestDir {
        let d = crate::testdir::new(&format!("env-{name}"));
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// Ourselves, as the session user.
    fn me() -> SessionUser {
        SessionUser {
            uid: Uid::current().as_raw(),
            gid: Gid::current().as_raw(),
            name: "ana".into(),
            home: "/home/ana".into(),
            groups: Vec::new(),
        }
    }

    fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn desktop(lock: Option<&str>) -> DesktopEnv {
        DesktopEnv {
            wayland_display: lock.map(str::to_string),
        }
    }

    #[test]
    fn finds_the_session_in_the_runtime_dir() {
        let rt = runtime("full");
        let _w0 = UnixListener::bind(rt.join("wayland-0")).unwrap();
        let _w1 = UnixListener::bind(rt.join("wayland-1")).unwrap();
        fs::write(rt.join("wayland-1.lock"), "").unwrap();
        let _bus = UnixListener::bind(rt.join("bus")).unwrap();
        let env = session_env_in(&me(), Some(&desktop(Some("wayland-1"))), &rt);
        assert_eq!(get(&env, "XDG_RUNTIME_DIR"), rt.to_str());
        // The desktop's own display wins over the first one by name.
        assert_eq!(get(&env, "WAYLAND_DISPLAY"), Some("wayland-1"));
        assert_eq!(get(&env, "XDG_SESSION_TYPE"), Some("wayland"));
        assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), Some("KDE"));
        let bus = format!("unix:path={}/bus", rt.display());
        assert_eq!(get(&env, "DBUS_SESSION_BUS_ADDRESS"), Some(bus.as_str()));
        assert_eq!(get(&env, "HOME"), Some("/home/ana"));
        assert_eq!(get(&env, "PATH"), Some("/home/ana/.local/bin:/usr/local/bin:/usr/bin:/bin"));
        // Nothing of ours leaks in (the test runner's own variables).
        let keys: Vec<_> = env.iter().map(|(k, _)| k.as_str()).collect();
        for k in &keys {
            assert!(
                [
                    "HOME", "USER", "LOGNAME", "PATH", "LANG", "LC_ALL", "XDG_RUNTIME_DIR",
                    "DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY",
                    "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP",
                ]
                .contains(k),
                "{k} leaked"
            );
        }
        // Without a desktop: the first socket by name, and no desktop variables.
        let env = session_env_in(&me(), None, &rt);
        assert_eq!(get(&env, "WAYLAND_DISPLAY"), Some("wayland-0"));
        assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), None);
        let _ = fs::remove_dir_all(&rt);
    }

    #[test]
    fn a_plasma_session_gets_kde_variables() {
        let rt = runtime("plasma");
        let _w = UnixListener::bind(rt.join("wayland-0")).unwrap();
        let env = session_env_in(&me(), Some(&desktop(Some("wayland-0"))), &rt);
        assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), Some("KDE"));
        assert_eq!(get(&env, "XDG_SESSION_TYPE"), Some("wayland"));
        assert_eq!(get(&env, "WAYLAND_DISPLAY"), Some("wayland-0"));
        let _ = fs::remove_dir_all(&rt);
    }

    #[test]
    fn takes_nothing_on_trust() {
        let rt = runtime("bad");
        // Not sockets, or symlinks to one: skipped.
        fs::write(rt.join("wayland-0"), "").unwrap();
        let _real = UnixListener::bind(rt.join("real")).unwrap();
        symlink(rt.join("real"), rt.join("wayland-1")).unwrap();
        // A hint that would leave the directory, a name that isn't a socket, or an
        // absolute path: none is taken.
        for lock in ["../real", "wayland-0", "/x"] {
            let env = session_env_in(&me(), Some(&desktop(Some(lock))), &rt);
            assert_eq!(get(&env, "XDG_RUNTIME_DIR"), rt.to_str());
            assert_eq!(get(&env, "WAYLAND_DISPLAY"), None, "{lock:?}");
            assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), None);
            assert_eq!(get(&env, "DBUS_SESSION_BUS_ADDRESS"), None);
        }
        // Another user's runtime dir, an open one, or none: no session at all.
        let mut other = me();
        other.uid += 1;
        let _w = UnixListener::bind(rt.join("wayland-2")).unwrap();
        let env = session_env_in(&other, Some(&desktop(Some("wayland-2"))), &rt);
        assert_eq!(get(&env, "XDG_RUNTIME_DIR"), None);
        assert_eq!(get(&env, "WAYLAND_DISPLAY"), None);
        assert_eq!(get(&env, "HOME"), Some("/home/ana"), "the basics are always there");
        fs::set_permissions(&rt, fs::Permissions::from_mode(0o755)).unwrap();
        let env = session_env_in(&me(), Some(&desktop(Some("wayland-2"))), &rt);
        assert_eq!(get(&env, "XDG_RUNTIME_DIR"), None, "group/others can enter it");
        let env = session_env_in(&me(), None, &rt.join("missing"));
        assert_eq!(get(&env, "XDG_RUNTIME_DIR"), None);
        // Back to private: the good socket is taken.
        fs::set_permissions(&rt, fs::Permissions::from_mode(0o700)).unwrap();
        let env = session_env_in(&me(), Some(&desktop(Some("wayland-2"))), &rt);
        assert_eq!(get(&env, "WAYLAND_DISPLAY"), Some("wayland-2"));
        assert_eq!(get(&env, "XDG_CURRENT_DESKTOP"), Some("KDE"));
        let _ = fs::remove_dir_all(&rt);
    }
}
