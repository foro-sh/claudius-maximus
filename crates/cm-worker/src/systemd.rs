//! `sd_notify`, by hand: three datagrams and a socket path out of the
//! environment.
//!
//! The worker runs under systemd (`claudius@.service`) and never anywhere
//! else, so `systemctl status claudius@claudius-maximus` is where somebody
//! looks first. Without this it answers "active (running)" and nothing more,
//! which is the same answer for a worker shipping a PR, a worker waiting out a
//! five-hour window and a worker wedged on a dead socket. With it, the unit
//! carries the same line the journal heartbeat writes.
//!
//! The watchdog is the other half: systemd kills and restarts a worker whose
//! pings stop. That is a liveness check on the *process*, not on the backlog -
//! the pings come from the heartbeat thread, which keeps beating while a run
//! blocks for hours, because that run is allowed to. What it catches is the
//! process that is still there but no longer anybody home.
//!
//! No `libsystemd`: the protocol is newline-separated `KEY=value` in a
//! datagram, and a dependency for that would be sillier than the thirty lines
//! below.

use std::io::ErrorKind;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub struct Systemd {
    socket: Option<UnixDatagram>,
    watchdog: Option<Duration>,
    /// A dead notify socket must not turn into a line per tick forever.
    complained: AtomicBool,
}

impl Systemd {
    /// Reads `NOTIFY_SOCKET` and `WATCHDOG_USEC`.
    ///
    /// The variables are left in the environment rather than removed: under
    /// `#[tokio::main]` the runtime's threads already exist by the time any of
    /// this runs, and removing an environment variable with other threads
    /// alive is unsound. The `claude` child is kept away from the notify
    /// socket where the child is spawned instead (see `claude_cli`), which is
    /// the only place that inherits it.
    pub fn from_env() -> Self {
        let socket = std::env::var("NOTIFY_SOCKET")
            .ok()
            .filter(|s| !s.is_empty());

        // `WATCHDOG_PID`, when set, names who systemd meant the watchdog for.
        // Anyone else pinging it is answering for a process they are not.
        let ours = std::env::var("WATCHDOG_PID")
            .ok()
            .is_none_or(|pid| pid.parse() == Ok(std::process::id()));
        let watchdog = std::env::var("WATCHDOG_USEC")
            .ok()
            .filter(|_| ours)
            .and_then(|usec| usec.parse::<u64>().ok())
            .filter(|usec| *usec > 0)
            .map(Duration::from_micros);

        match socket {
            Some(path) => Systemd {
                socket: connect(&path),
                watchdog,
                complained: AtomicBool::new(false),
            },
            None => Systemd::detached(),
        }
    }

    /// A worker nobody is supervising: every call below is a no-op. This is
    /// the shape a foreground run takes (the device-flow login in the README,
    /// and every test).
    pub fn detached() -> Self {
        Systemd {
            socket: None,
            watchdog: None,
            complained: AtomicBool::new(false),
        }
    }

    pub fn supervised(&self) -> bool {
        self.socket.is_some()
    }

    /// How often systemd wants to hear from us, when it is watching.
    pub fn watchdog_interval(&self) -> Option<Duration> {
        self.watchdog
    }

    /// "Started." Sent before the GitHub device flow rather than after it: the
    /// flow waits on a human opening a URL, and a unit that counts that as
    /// part of starting up is a unit systemd kills at `TimeoutStartSec`.
    pub fn ready(&self) {
        self.send("READY=1");
    }

    /// The line `systemctl status` shows under the unit.
    pub fn status(&self, text: &str) {
        // Newlines would be read as the start of another field.
        self.send(&format!("STATUS={}", text.replace('\n', " ")));
    }

    pub fn ping(&self) {
        if self.watchdog.is_some() {
            self.send("WATCHDOG=1");
        }
    }

    fn send(&self, message: &str) {
        let Some(socket) = &self.socket else {
            return;
        };
        match socket.send(message.as_bytes()) {
            Ok(_) => {}
            Err(err) => {
                if !self.complained.swap(true, Ordering::Relaxed) {
                    println!("systemd notify failed (ignored from here on): {err}");
                }
            }
        }
    }
}

/// Connects a datagram socket to systemd's notify socket, once, so every
/// message afterwards is a bare `send`.
fn connect(path: &str) -> Option<UnixDatagram> {
    let socket = match UnixDatagram::unbound() {
        Ok(socket) => socket,
        Err(err) => {
            println!("systemd notify socket unusable (ignored): {err}");
            return None;
        }
    };
    // A leading '@' is systemd's spelling of the abstract namespace, where the
    // name starts with a NUL byte instead of living on the filesystem.
    let connected = if let Some(name) = path.strip_prefix('@') {
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name)
                .and_then(|addr| socket.connect_addr(&addr))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = name;
            Err(std::io::Error::new(
                ErrorKind::Unsupported,
                "abstract unix sockets are Linux-only",
            ))
        }
    } else {
        socket.connect(path)
    };
    match connected {
        Ok(()) => Some(socket),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            println!("systemd notify socket {path} is not there (ignored): {err}");
            None
        }
        Err(err) => {
            println!("systemd notify socket {path} could not be reached (ignored): {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A notify socket of our own, in the same place systemd would put one.
    struct Listener {
        socket: UnixDatagram,
        path: std::path::PathBuf,
    }

    impl Listener {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cm-notify-{}-{name}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Listener {
                socket: UnixDatagram::bind(&path).unwrap(),
                path,
            }
        }

        fn heard(&self) -> String {
            let mut buf = [0u8; 256];
            let read = self.socket.recv(&mut buf).unwrap();
            String::from_utf8_lossy(&buf[..read]).into_owned()
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn talking_to(listener: &Listener, watchdog: Option<Duration>) -> Systemd {
        Systemd {
            socket: connect(&listener.path.to_string_lossy()),
            watchdog,
            complained: AtomicBool::new(false),
        }
    }

    #[test]
    fn systemd_hears_ready_and_the_status_line() {
        let listener = Listener::new("ready");
        let systemd = talking_to(&listener, None);
        assert!(systemd.supervised());

        systemd.ready();
        assert_eq!(listener.heard(), "READY=1");

        systemd.status("implementing foro-sh/foro#7 for 12m");
        assert_eq!(
            listener.heard(),
            "STATUS=implementing foro-sh/foro#7 for 12m"
        );
    }

    #[test]
    fn a_status_line_never_carries_a_second_field() {
        let listener = Listener::new("newline");
        let systemd = talking_to(&listener, None);

        systemd.status("waiting\nREADY=0");
        assert_eq!(
            listener.heard(),
            "STATUS=waiting READY=0",
            "a newline in the status would end the field and start another"
        );
    }

    #[test]
    fn the_watchdog_is_only_pinged_when_systemd_asked_for_it() {
        let listener = Listener::new("watchdog");

        let unwatched = talking_to(&listener, None);
        unwatched.ping();
        unwatched.status("still here");
        assert_eq!(
            listener.heard(),
            "STATUS=still here",
            "an unasked-for ping would be the only thing in the queue"
        );

        let watched = talking_to(&listener, Some(Duration::from_secs(180)));
        watched.ping();
        assert_eq!(listener.heard(), "WATCHDOG=1");
        assert_eq!(watched.watchdog_interval(), Some(Duration::from_secs(180)));
    }

    #[test]
    fn a_worker_nobody_supervises_says_nothing_and_does_not_fail() {
        let detached = Systemd::detached();
        detached.ready();
        detached.status("implementing");
        detached.ping();
        assert!(!detached.supervised());
        assert_eq!(detached.watchdog_interval(), None);
    }

    #[test]
    fn a_notify_socket_that_is_not_there_is_not_fatal() {
        assert!(connect("/nonexistent/cm-notify-socket").is_none());
    }
}
