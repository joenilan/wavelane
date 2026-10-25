//! One Wavelane per desktop session. The first instance holds an exclusive lock
//! in the user's runtime directory and listens on a private socket next to
//! it. A later launch (application menu, terminal, a second login autostart)
//! asks that instance to open the map and exits before it touches PipeWire.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::{Duration, Instant},
};

#[cfg(not(feature = "dev-shortcut"))]
const LOCK: &str = "wavelane.lock";
#[cfg(feature = "dev-shortcut")]
const LOCK: &str = "wavelane-dev.lock";
#[cfg(not(feature = "dev-shortcut"))]
const SOCKET: &str = "wavelane-instance.sock";
#[cfg(feature = "dev-shortcut")]
const SOCKET: &str = "wavelane-dev-instance.sock";
/// A starting first instance binds its socket right after taking the lock.
const CONNECT_WAIT: Duration = Duration::from_secs(3);

/// What a launch should do.
pub enum Claim {
    /// This is the only Wavelane: keep the guard for the process lifetime.
    Primary(Instance),
    /// Another Wavelane is running; the result of asking it to show (or to stay
    /// hidden). Never start a second copy, even when it did not answer.
    Running(io::Result<()>),
}

/// Held by the running Wavelane.
pub struct Instance {
    _lock: File,
    listener: UnixListener,
    socket: PathBuf,
}

/// A request from a later launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Bring the window up (an ordinary launch), with the launcher's
    /// xdg-activation token when it gave one, so Wayland raises the window.
    Show(Option<String>),
    /// A hidden launch (login autostart): nothing to show.
    Hidden,
    /// `wavelane --map` (a keyboard shortcut): open or close the map.
    Map(Option<String>),
}

fn runtime_dir() -> io::Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))
}

/// Become the running Wavelane, or hand this launch to the one already running.
pub fn claim(hidden: bool, map: bool) -> io::Result<Claim> {
    let dir = runtime_dir()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(LOCK))?;
    let socket = dir.join(SOCKET);
    match lock.try_lock() {
        Ok(()) => {
            // Holding the lock, any socket left at this path is stale.
            if fs::symlink_metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) {
                fs::remove_file(&socket)?;
            }
            let listener = UnixListener::bind(&socket)?;
            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            Ok(Claim::Primary(Instance {
                _lock: lock,
                listener,
                socket,
            }))
        }
        Err(fs::TryLockError::WouldBlock) => Ok(Claim::Running(forward(
            &socket,
            {
                let token = std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| {
                    !t.is_empty() && t.len() < 256 && t.bytes().all(|b| b.is_ascii_graphic())
                });
                if map {
                    Request::Map(token)
                } else if hidden {
                    Request::Hidden
                } else {
                    Request::Show(token)
                }
            },
        ))),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

fn forward(socket: &PathBuf, request: Request) -> io::Result<()> {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if started.elapsed() < CONNECT_WAIT => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let line = match request {
        Request::Show(Some(token)) => format!("show {token}\n"),
        Request::Show(None) => "show\n".to_owned(),
        Request::Hidden => "hidden\n".to_owned(),
        Request::Map(Some(token)) => format!("map {token}\n"),
        Request::Map(None) => "map\n".to_owned(),
    };
    stream.write_all(line.as_bytes())?;
    // Wait for the acknowledgement so the request is never lost.
    let mut reply = [0u8; 3];
    stream.read_exact(&mut reply)?;
    Ok(())
}

impl Instance {
    /// Pending requests from later launches (never blocks).
    pub fn requests(&self) -> Vec<Request> {
        let mut out = Vec::new();
        while let Ok((mut stream, _)) = self.listener.accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut line = [0u8; 300];
            let n = stream.read(&mut line).unwrap_or(0);
            let text = String::from_utf8_lossy(&line[..n]);
            let text = text.trim_end();
            let token = |prefix: &str| {
                text.strip_prefix(prefix)
                    .map(str::trim)
                    .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()))
                    .map(str::to_owned)
            };
            let request = if text.starts_with("hidden") {
                Request::Hidden
            } else if text == "map" || text.starts_with("map ") {
                Request::Map(token("map "))
            } else {
                Request::Show(token("show "))
            };
            let _ = stream.write_all(b"ok\n");
            out.push(request);
        }
        out
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}
