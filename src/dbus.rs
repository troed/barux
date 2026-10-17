//! The session-bus connection barux keeps for the KDE desktop source (and, later,
//! for notifications). Barux may run as root, so the connection is made with the
//! address the *session user's* environment names, never root's own bus.
//!
//! The `zbus` blocking API runs the connection's executor on its own thread. A
//! signal watcher (`Bus::watch`) and the method barux serves (`Bus::serve`) push
//! [`Notice`]s onto a shared queue and write an `eventfd`, so the daemon's epoll
//! loop wakes to them without polling — the same shape as `src/gifpick.rs`.

use anyhow::{Context, Result, anyhow};
use nix::sys::eventfd::{EfdFlags, EventFd};
use std::{
    collections::VecDeque,
    os::fd::{AsFd, BorrowedFd},
    sync::{Arc, Mutex},
    time::Duration,
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    object_server::Interface,
    zvariant::{OwnedValue, StructureBuilder, Value},
};

/// A hung desktop must not freeze the bar: calls give up after this.
const CALL_TIMEOUT: Duration = Duration::from_millis(500);

/// What the bus wants the main loop to look at, in arrival order.
#[derive(Debug, PartialEq)]
pub enum Notice {
    /// A subscribed signal arrived: the desktop state may have changed.
    Signal,
    /// A KWin script pushed a snapshot (its raw JSON).
    Snapshot(String),
}

/// State the connection's threads share with the main loop: a queue of notices and
/// an eventfd written whenever one is queued.
struct Shared {
    notices: Mutex<VecDeque<Notice>>,
    wake: EventFd,
}

impl Shared {
    /// Queues `notice` and wakes the loop. A full eventfd counter just means a
    /// wake-up is already pending, so a failed write is not an error.
    fn push(&self, notice: Notice) {
        if let Ok(mut q) = self.notices.lock() {
            q.push_back(notice);
        }
        let _ = self.wake.write(1);
    }
}

/// A clonable handle that queues [`Notice`]s for the main loop, for objects the bus
/// serves (the KWin script's `Snapshot`).
#[derive(Clone)]
pub struct Notifier {
    shared: Arc<Shared>,
}

impl Notifier {
    pub fn push(&self, notice: Notice) {
        self.shared.push(notice);
    }
}

/// A `zbus` connection to the session user's bus, with a wake pipe for epoll.
pub struct Bus {
    conn: Connection,
    shared: Arc<Shared>,
}

impl Bus {
    /// Connects to the session bus named by `env` (its `DBUS_SESSION_BUS_ADDRESS`).
    pub fn connect(env: &[(String, String)]) -> Result<Bus> {
        let address = env
            .iter()
            .find(|(k, _)| k == "DBUS_SESSION_BUS_ADDRESS")
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| anyhow!("no session bus in the environment"))?;
        let conn = Builder::address(address)
            .context("session bus address")?
            .method_timeout(CALL_TIMEOUT)
            .build()
            .context("connecting to the session bus")?;
        let wake = EventFd::from_flags(EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK)
            .context("eventfd for the bus")?;
        Ok(Bus {
            conn,
            shared: Arc::new(Shared {
                notices: Mutex::new(VecDeque::new()),
                wake,
            }),
        })
    }

    /// The underlying connection, for building typed `zbus` proxies.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// A handle to queue notices from an object the bus serves.
    pub fn notifier(&self) -> Notifier {
        Notifier {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The wake pipe: readable (`EPOLLIN`) once a notice is queued.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.shared.wake.as_fd()
    }

    /// Clears the wake pipe (after the notices have been taken).
    pub fn drain(&mut self) {
        let _ = self.shared.wake.read();
    }

    /// Takes the queued notices, oldest first.
    pub fn take(&self) -> Vec<Notice> {
        self.shared
            .notices
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// Calls `method` on `iface` at `path` with `args`, returning the reply's one
    /// body value (a placeholder when the method returns nothing).
    pub fn call(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        method: &str,
        args: &[Value<'_>],
    ) -> Result<OwnedValue> {
        let mut builder = StructureBuilder::new();
        for a in args {
            builder = builder.append_field(a.try_clone()?);
        }
        let body = builder.build().context("building the call arguments")?;
        let reply = self
            .conn
            .call_method(Some(dest), path, Some(iface), method, &body)
            .with_context(|| format!("{iface}.{method}"))?;
        let body = reply.body();
        if body.is_empty() {
            // A void method (e.g. a dispatch): no value to give back.
            return Ok(OwnedValue::from(0u8));
        }
        body.deserialize::<OwnedValue>().map_err(Into::into)
    }

    /// Serves `iface` at `path` and takes `name` on the bus.
    pub fn serve<I>(&self, name: &str, path: &str, iface: I) -> Result<()>
    where
        I: Interface + Send + Sync + 'static,
    {
        self.conn
            .object_server()
            .at(path, iface)
            .with_context(|| format!("serving {path}"))?;
        self.conn
            .request_name(name)
            .with_context(|| format!("owning {name}"))?;
        Ok(())
    }

    /// Watches `signal` on `iface` at `path` (owned by `dest`), waking the loop
    /// with [`Notice::Signal`] on each. The watcher thread ends with the connection.
    pub fn watch(&self, dest: &str, path: &str, iface: &str, signal: &str) -> Result<()> {
        let proxy = Proxy::new(
            &self.conn,
            dest.to_string(),
            path.to_string(),
            iface.to_string(),
        )
        .with_context(|| format!("proxy for {iface} at {path}"))?;
        let shared = Arc::clone(&self.shared);
        let signal = signal.to_string();
        std::thread::Builder::new()
            .name("barux-dbus-signals".into())
            .spawn(move || {
                let Ok(iter) = proxy.receive_signal(signal.as_str()) else {
                    return;
                };
                for _ in iter {
                    shared.push(Notice::Signal);
                }
            })
            .context("starting the signal watcher")?;
        Ok(())
    }
}
