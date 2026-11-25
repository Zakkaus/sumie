//! Desktop notifications over the session bus (`org.freedesktop.Notifications`).
//!
//! Connecting, authenticating and sending block, so a worker thread owns the socket and the
//! main thread only queues requests: a full queue drops the request, never waits. No D-Bus crate
//! is linked and no bus is ever started; the address comes from the environment.

mod wire;

use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use sumie_core::notify::Notification;

const QUEUE: usize = 4;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_AFTER: Duration = Duration::from_secs(30);

type Result<T> = std::io::Result<T>;

pub(crate) struct Notifier {
    requests: SyncSender<Notification>,
}

impl Notifier {
    /// Resolve the bus address from the environment now; connect on the first request.
    pub(crate) fn new() -> Self {
        let address = session_address(
            std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        );
        let (requests, queue) = sync_channel(QUEUE);
        let spawned = std::thread::Builder::new()
            .name("sumie-notify".into())
            .spawn(move || serve(&queue, &address));
        if let Err(error) = spawned {
            tracing::warn!(%error, "desktop notifications are unavailable");
        }
        Self { requests }
    }

    pub(crate) fn send(&self, request: Notification) {
        if let Err(TrySendError::Full(_)) = self.requests.try_send(request) {
            tracing::warn!("dropped a desktop notification: queue full");
        }
    }
}

fn serve(queue: &Receiver<Notification>, address: &str) {
    let mut connection = None;
    let mut backoff = Backoff::default();
    while let Ok(request) = queue.recv() {
        deliver(&mut connection, &mut backoff, address, &request);
    }
}

/// While the bus is unreachable, requests are dropped until `RETRY_AFTER` has passed since the
/// last failure, and one failure burst is logged once: an absent bus costs neither a connect
/// per notification nor a log line each.
#[derive(Default)]
struct Backoff {
    failed_at: Option<Instant>,
    logged: bool,
}

impl Backoff {
    fn blocked(&self, now: Instant) -> bool {
        self.failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < RETRY_AFTER)
    }

    /// Record a failure; true when this burst has not been logged yet.
    fn failed(&mut self, now: Instant) -> bool {
        self.failed_at = Some(now);
        !std::mem::replace(&mut self.logged, true)
    }

    fn note(&mut self, now: Instant, error: &std::io::Error, what: &str) {
        if self.failed(now) {
            tracing::warn!(%error, "{what}");
        }
    }

    fn recovered(&mut self) {
        *self = Self::default();
    }
}

fn connect(backoff: &mut Backoff, address: &str) -> Option<Connection> {
    let now = Instant::now();
    if backoff.blocked(now) {
        return None;
    }
    Connection::open(address)
        .inspect_err(|error| backoff.note(now, error, "could not reach the session bus"))
        .ok()
}

/// Write the request, then wait for the reply. A failed write on a reused connection is retried
/// once on a fresh one, since the connection may have died while idle; a failed reply drops the
/// connection without resending.
fn deliver(
    connection: &mut Option<Connection>,
    backoff: &mut Backoff,
    address: &str,
    request: &Notification,
) {
    for _ in 0..2 {
        let reused = connection.is_some();
        let Some(mut open) = connection.take().or_else(|| connect(backoff, address)) else {
            return;
        };
        match open.send(request) {
            Ok(serial) => {
                if let Err(error) = open.reply(serial) {
                    tracing::warn!(%error, "no reply to a desktop notification");
                } else {
                    backoff.recovered();
                    *connection = Some(open);
                }
                return;
            }
            Err(_) if reused => (),
            Err(error) => {
                backoff.note(
                    Instant::now(),
                    &error,
                    "could not send a desktop notification",
                );
                return;
            }
        }
    }
}

/// The first `unix:` transport in a D-Bus address, as a socket address.
fn socket_address(address: &str) -> Option<SocketAddr> {
    address.split(';').find_map(|entry| {
        let options = entry.strip_prefix("unix:")?;
        options.split(',').find_map(|option| {
            let (key, value) = option.split_once('=')?;
            let value = unescape(value);
            match key {
                "path" => SocketAddr::from_pathname(std::str::from_utf8(&value).ok()?).ok(),
                "abstract" => SocketAddr::from_abstract_name(&value).ok(),
                _ => None,
            }
        })
    })
}

fn unescape(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| value.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        out.push(escaped.unwrap_or(bytes[at]));
        at += if escaped.is_some() { 3 } else { 1 };
    }
    out
}

/// `DBUS_SESSION_BUS_ADDRESS`, else the systemd-style socket in the runtime directory.
fn session_address(environment: Option<&str>, runtime_dir: Option<&str>) -> String {
    match (environment.filter(|value| !value.is_empty()), runtime_dir) {
        (Some(address), _) => address.to_owned(),
        (None, Some(dir)) => format!("unix:path={dir}/bus"),
        (None, None) => String::new(),
    }
}

struct Connection {
    socket: UnixStream,
    serial: u32,
    unread: Vec<u8>,
}

impl Connection {
    fn open(address: &str) -> Result<Self> {
        let target = socket_address(address)
            .ok_or_else(|| std::io::Error::other("no unix transport in the session bus address"))?;
        let socket = UnixStream::connect_addr(&target)?;
        socket.set_read_timeout(Some(IO_TIMEOUT))?;
        socket.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut connection = Self {
            socket,
            serial: 1,
            unread: Vec::new(),
        };
        let uid = std::fs::metadata("/proc/self")?.uid();
        connection.socket.write_all(&wire::auth_line(uid))?;
        let mut line = [0; 128];
        let read = connection.socket.read(&mut line)?;
        if !line[..read].starts_with(b"OK ") {
            return Err(std::io::Error::other(
                "the bus refused EXTERNAL authentication",
            ));
        }
        connection.socket.write_all(wire::BEGIN)?;
        let serial = connection.next_serial();
        connection.socket.write_all(&wire::hello(serial))?;
        // Wait for the reply, so a connection that cannot register is not reused.
        if connection.await_reply(serial)? == wire::ERROR {
            return Err(std::io::Error::other("the bus rejected Hello"));
        }
        Ok(connection)
    }

    fn next_serial(&mut self) -> u32 {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1).max(1);
        serial
    }

    fn read_message(&mut self) -> Result<Vec<u8>> {
        loop {
            if let Some((length, _)) = wire::frame(&self.unread) {
                return Ok(self.unread.drain(..length).collect());
            }
            let mut chunk = [0; 1024];
            let read = self.socket.read(&mut chunk)?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.unread.extend_from_slice(&chunk[..read]);
        }
    }

    /// The type of the reply to call `serial`, skipping signals such as `NameAcquired` and
    /// replies to anything else.
    fn await_reply(&mut self, serial: u32) -> Result<u8> {
        loop {
            let message = self.read_message()?;
            let kind = message[1];
            if kind != wire::SIGNAL && wire::reply_serial(&message) == Some(serial) {
                return Ok(kind);
            }
        }
    }

    /// Send the request and return the serial of the call.
    fn send(&mut self, request: &Notification) -> Result<u32> {
        // A daemon shows the summary in bold, so a body-only request still needs one.
        let summary = if request.title.is_empty() {
            "sumie"
        } else {
            &request.title
        };
        let serial = self.next_serial();
        self.socket
            .write_all(&wire::notify(serial, summary, &request.body))?;
        Ok(serial)
    }

    /// The reply carries the notification id, which nothing uses; only refusals matter.
    fn reply(&mut self, serial: u32) -> Result<()> {
        if self.await_reply(serial)? == wire::ERROR {
            tracing::warn!("org.freedesktop.Notifications refused a notification");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_connection_blocks_retries_until_the_backoff_passes_and_logs_once() {
        let start = Instant::now();
        let mut backoff = Backoff::default();
        assert!(!backoff.blocked(start));
        assert!(backoff.failed(start));
        // (seconds after the first failure, blocked)
        for (secs, blocked) in [(1, true), (29, true), (30, false)] {
            assert_eq!(backoff.blocked(start + Duration::from_secs(secs)), blocked);
        }
        assert!(
            !backoff.failed(start + Duration::from_secs(30)),
            "same burst"
        );
        backoff.recovered();
        assert!(backoff.failed(start + Duration::from_secs(90)), "new burst");
    }

    #[test]
    fn bus_addresses_resolve_to_socket_paths() {
        let path = |address: &str| {
            socket_address(address).and_then(|socket| socket.as_pathname().map(ToOwned::to_owned))
        };
        let named = |address: &str| {
            socket_address(address).and_then(|socket| socket.as_abstract_name().map(<[u8]>::to_vec))
        };
        assert_eq!(
            path("unix:path=/run/user/1000/bus,guid=ab"),
            Some("/run/user/1000/bus".into())
        );
        assert_eq!(path("tcp:host=x;unix:path=/a%20b"), Some("/a b".into()));
        assert_eq!(
            named("unix:guid=1,abstract=/tmp/dbus-X"),
            Some(b"/tmp/dbus-X".to_vec())
        );
        assert!(socket_address("tcp:host=localhost,port=1").is_none());
        assert_eq!(
            session_address(None, Some("/run/user/7")),
            "unix:path=/run/user/7/bus"
        );
        assert_eq!(
            session_address(Some("unix:path=/x"), Some("/run/user/7")),
            "unix:path=/x"
        );
    }
}
