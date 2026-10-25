//! Update checks against the public GitHub releases of Wavelane, with
//! click-to-download and click-to-install (ported from Jawjack's
//! `crates/desktop/src/updates.rs`, itself from Haiwire's update flow).
//!
//! Nothing downloads or installs without a click. With the mode
//! **Notify me** (the default) Wavelane reads the release list [`STARTUP_DELAY`]
//! after startup and then every [`POLL_INTERVAL`]; **Off** reads it only when
//! asked ([`Updates::check_now`]). A newer version becomes
//! [`Phase::Available`] with its release notes and is announced once per
//! session ([`Updates::take_announcement`]).
//!
//! Feed: `GET https://api.github.com/repos/joenilan/wavelane/releases?per_page=20`
//! (`Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`,
//! `User-Agent: Wavelane/<version>`, conditional with the last `ETag`, which keeps
//! repeat checks outside GitHub's unauthenticated rate limit). Releases are
//! published as pre-releases, so `/releases/latest` is not used. Drafts are
//! ignored; the candidate is the highest strict SemVer `v<version>` tag. Its
//! `body` is untrusted plain text, bounded like Jawjack's notes.
//!
//! Package: chosen by how this copy was installed ([`InstallKind`]); the
//! asset name must match exactly (`wavelane-<v>-<n>.fc<NN>.x86_64.rpm`,
//! `wavelane_<v>-<n>_amd64.deb`, `wavelane-<v>-x86_64-linux.tar.gz`). Asset URLs in
//! the feed are ignored: the download URL is derived from the fixed release
//! base and the validated tag and name. Size comes from the feed; SHA-256 from
//! the release's `SHA256SUMS`. Size and hash are enforced while streaming into
//! a private (0700) `$XDG_CACHE_HOME/wavelane/update-*` folder and checked again
//! before installing; any mismatch fails closed and deletes the folder.
//!
//! Transport: HTTPS only, OS certificate store, no cookies or authentication,
//! bounded size and time. Redirects (GitHub serves downloads through its
//! asset host) are followed only to `github.com`, `api.github.com`,
//! `objects.githubusercontent.com` and `release-assets.githubusercontent.com`.
//!
//! Install: RPM via `pkexec dnf install -y <file>`, Deb via
//! `pkexec apt-get install -y <file>` (the system asks for the password),
//! tarball (per-user `~/.local` install) by replacing `~/.local/bin/wavelane`,
//! its desktop entry and icon atomically, without a password. Other copies (a
//! build output, `/usr/local`) may check but not install. Once installed,
//! [`Updates::relaunch`] starts the new Wavelane after this process has exited;
//! the caller then stops routing and quits.

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant, SystemTime},
};

/// The first background check waits until startup work has settled.
pub const STARTUP_DELAY: Duration = Duration::from_secs(15);
/// Background checks repeat this often.
pub const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
pub const RELEASE_PAGE: &str = "https://github.com/joenilan/wavelane/releases";

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const FEED_URL: &str =
    "https://api.github.com/repos/joenilan/wavelane/releases?per_page=20";
const DOWNLOAD_BASE: &str = "https://github.com/joenilan/wavelane/releases/download";
const SUMS_NAME: &str = "SHA256SUMS";
const ALLOWED_HOSTS: [&str; 4] = [
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;
/// Twenty releases with their notes; the notes themselves are bounded below.
const MAX_FEED_BYTES: usize = 1024 * 1024;
const MAX_SUMS_BYTES: usize = 64 * 1024;
const MAX_NOTES_BYTES: usize = 64 * 1024;
const MAX_NOTE_LINES: usize = 80;
const MAX_NOTE_CHARS: usize = 400;
const MAX_VALIDATOR_LENGTH: usize = 256;
const MAX_VERSION_LENGTH: usize = 128;
/// Hard cap on a package download, independent of the feed's size.
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SMALL_FILE_BYTES: u64 = 1024 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const FOLDER_PREFIX: &str = "update-";

const TAR_BIN: [&str; 3] = ["usr", "bin", "wavelane"];
const TAR_DESKTOP: [&str; 4] = ["usr", "share", "applications", "digital.zombie.wavelane.desktop"];
const TAR_ICON: [&str; 6] = ["usr", "share", "icons", "hicolor", "scalable", "apps"];
const ICON_NAME: &str = "digital.zombie.wavelane.svg";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    Off,
    #[default]
    Notify,
}

/// How this copy of Wavelane was installed, which decides the package and installer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    Rpm,
    Deb,
    /// Arch Linux (pacman).
    Pacman,
    /// `install.sh` for this user (`~/.local`).
    Tarball,
    /// Anything else: may check, never installs.
    Unmanaged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading {
        percent: u64,
    },
    /// Downloaded and verified; waiting for Install.
    Ready,
    Installing,
    /// Installed; waiting for [`Updates::relaunch`].
    Installed,
    Failed,
}

/// What the UI shows. (The flyout shows a line, not the notes or kind.)
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct UpdateView {
    pub phase: Phase,
    pub mode: UpdateMode,
    pub current: &'static str,
    pub latest: Option<String>,
    pub notes: Vec<String>,
    pub error: Option<String>,
    pub kind: InstallKind,
    /// Why this copy cannot install updates, when it cannot.
    pub ineligible: Option<&'static str>,
}

// ------------------------------------------------------------------ state

/// The package published for this install kind (name and size from the feed).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Package {
    name: String,
    size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    version: String,
    notes: Vec<String>,
    package: Option<Package>,
}

/// The last 200 response of the feed and its `ETag`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FeedCache {
    etag: String,
    body: Vec<u8>,
}

struct CheckOutcome {
    result: Result<Option<Candidate>, String>,
    cache: Option<FeedCache>,
}

/// A verified package held in its private folder.
struct Held {
    version: String,
    dir: PathBuf,
    path: PathBuf,
    file: File,
    package: Package,
    sha256: String,
}

struct Downloaded {
    dir: PathBuf,
    path: PathBuf,
    file: File,
    sha256: String,
}

enum DownloadEvent {
    Progress(u64),
    Done(Result<Downloaded, String>),
}

enum InstallFailure {
    /// The held file no longer verifies; it was not used.
    Rejected(String),
    Failed(String),
}

pub struct Updates {
    phase: Phase,
    mode: UpdateMode,
    kind: InstallKind,
    config: Option<PathBuf>,
    latest: Option<Candidate>,
    error: Option<String>,
    cache: Option<FeedCache>,
    held: Option<Held>,
    check_rx: Option<Receiver<CheckOutcome>>,
    download_rx: Option<Receiver<DownloadEvent>>,
    install_rx: Option<Receiver<Result<(), InstallFailure>>>,
    next_check: Option<Instant>,
    announced: Option<String>,
    announcement: Option<String>,
}

impl Updates {
    /// Reads the saved mode (`$XDG_CONFIG_HOME/wavelane/updates`) and detects the install kind.
    pub fn load() -> Self {
        let config = config_path();
        let mode = config.as_deref().map(read_mode).unwrap_or_default();
        let kind = detect_kind();
        if let Some(root) = cache_root() {
            discard_old_downloads(&root, None);
        }
        Self {
            phase: Phase::Idle,
            mode,
            kind,
            config,
            latest: None,
            error: None,
            cache: None,
            held: None,
            check_rx: None,
            download_rx: None,
            install_rx: None,
            next_check: (mode == UpdateMode::Notify).then(|| Instant::now() + STARTUP_DELAY),
            announced: None,
            announcement: None,
        }
    }

    pub fn view(&self) -> UpdateView {
        UpdateView {
            phase: self.phase.clone(),
            mode: self.mode,
            current: CURRENT_VERSION,
            latest: self.latest.as_ref().map(|c| c.version.clone()),
            notes: self
                .latest
                .as_ref()
                .map(|c| c.notes.clone())
                .unwrap_or_default(),
            error: self.error.clone(),
            kind: self.kind,
            ineligible: ineligible(self.kind),
        }
    }

    pub fn set_mode(&mut self, mode: UpdateMode) -> Result<(), String> {
        let path = self
            .config
            .as_deref()
            .ok_or("There is no settings folder to save the update mode in.")?;
        write_mode(path, mode)?;
        self.mode = mode;
        self.next_check = (mode == UpdateMode::Notify).then(Instant::now);
        Ok(())
    }

    pub fn check_now(&mut self) {
        self.begin_check();
    }

    /// Call about once a second: runs scheduled checks and collects worker
    /// results. Returns true when the view changed.
    pub fn poll(&mut self) -> bool {
        #[cfg(feature = "perf-harness")]
        if crate::benchmark::requested() { return false; }
        let checked = self.poll_check();
        let downloaded = self.poll_download();
        let installed = self.poll_install();
        let scheduled = self.poll_schedule();
        checked || downloaded || installed || scheduled
    }

    /// A newly available version, once per session.
    pub fn take_announcement(&mut self) -> Option<String> {
        self.announcement.take()
    }

    pub fn start_download(&mut self) {
        if self.phase != Phase::Available {
            return;
        }
        if let Some(reason) = ineligible(self.kind) {
            self.error = Some(reason.into());
            return;
        }
        let Some(candidate) = self.latest.clone() else {
            return;
        };
        let Some(package) = candidate.package else {
            self.error =
                Some("No package was published for this version. See the release page.".into());
            return;
        };
        let Some(root) = cache_root() else {
            self.error = Some("There is no cache folder to save the update in.".into());
            return;
        };
        self.error = None;
        self.phase = Phase::Downloading { percent: 0 };
        let version = candidate.version;
        let (tx, rx) = mpsc::channel();
        self.download_rx = Some(rx);
        std::thread::spawn(move || {
            let progress = tx.clone();
            let result = download(&root, &version, &package, &mut |received| {
                let _ = progress.send(DownloadEvent::Progress(received));
            });
            let _ = tx.send(DownloadEvent::Done(result));
        });
    }

    pub fn start_install(&mut self) {
        if self.phase != Phase::Ready {
            return;
        }
        if let Some(reason) = ineligible(self.kind) {
            self.error = Some(reason.into());
            return;
        }
        let Some(held) = self.held.as_ref() else {
            return;
        };
        let file = match held.file.try_clone() {
            Ok(file) => file,
            Err(_) => {
                self.error = Some("The downloaded package could not be reopened.".into());
                return;
            }
        };
        let job = InstallJob {
            kind: self.kind,
            version: held.version.clone(),
            path: held.path.clone(),
            file,
            size: held.package.size,
            sha256: held.sha256.clone(),
            work_root: held.dir.parent().map(Path::to_path_buf),
            home: home_dir(),
        };
        self.error = None;
        self.phase = Phase::Installing;
        let (tx, rx) = mpsc::channel();
        self.install_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(job.run());
        });
    }

    /// After [`Phase::Installed`]: start the new Wavelane once this process exits.
    /// `activation_token` (Wayland) lets its window open in front.
    pub fn relaunch(
        &self,
        args: &[String],
        activation_token: Option<String>,
    ) -> Result<(), String> {
        let exe = launch_target(self.kind, home_dir().as_deref())
            .ok_or("This copy of Wavelane cannot restart itself after an update.")?;
        spawn_relaunch(std::process::id(), &exe, args, activation_token.as_deref())
    }

    // ------------------------------------------------------------ workers

    fn busy(&self) -> bool {
        self.check_rx.is_some()
            || matches!(
                self.phase,
                Phase::Downloading { .. } | Phase::Installing | Phase::Installed
            )
    }

    fn begin_check(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        // A known update stays on screen while it is checked again quietly.
        if !matches!(self.phase, Phase::Available | Phase::Ready) {
            self.phase = Phase::Checking;
            self.error = None;
        }
        if self.mode == UpdateMode::Notify {
            self.next_check = Some(Instant::now() + POLL_INTERVAL);
        }
        let cache = self.cache.clone();
        let kind = self.kind;
        let (tx, rx) = mpsc::channel();
        self.check_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(fetch_release(cache, kind));
        });
        true
    }

    fn poll_schedule(&mut self) -> bool {
        let Some(due) = self.next_check else {
            return false;
        };
        if Instant::now() < due || self.mode != UpdateMode::Notify {
            return false;
        }
        if self.begin_check() {
            return true;
        }
        // Busy: skip this tick; nothing queues up.
        self.next_check = Some(Instant::now() + POLL_INTERVAL);
        false
    }

    fn poll_check(&mut self) -> bool {
        let Some(rx) = self.check_rx.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => CheckOutcome {
                result: Err("The update check stopped unexpectedly.".into()),
                cache: None,
            },
        };
        self.check_rx = None;
        if outcome.cache.is_some() {
            self.cache = outcome.cache;
        }
        self.apply_check(outcome.result);
        true
    }

    fn apply_check(&mut self, result: Result<Option<Candidate>, String>) {
        if matches!(
            self.phase,
            Phase::Downloading { .. } | Phase::Installing | Phase::Installed
        ) {
            return;
        }
        let holding = self.held.as_ref().map(|held| held.version.clone());
        match result {
            Ok(Some(candidate)) => {
                let same = self
                    .latest
                    .as_ref()
                    .is_some_and(|known| known.version == candidate.version);
                if !same {
                    self.error = None;
                }
                if holding.as_deref() == Some(candidate.version.as_str()) {
                    self.phase = Phase::Ready;
                } else {
                    // A newer release replaces an older held download.
                    if let Some(held) = self.held.take() {
                        discard(held);
                    }
                    self.phase = Phase::Available;
                }
                if self.announced.as_deref() != Some(candidate.version.as_str()) {
                    self.announced = Some(candidate.version.clone());
                    self.announcement = Some(candidate.version.clone());
                }
                self.latest = Some(candidate);
            }
            Ok(None) => {
                if self.held.is_some() {
                    self.phase = Phase::Ready;
                } else {
                    self.latest = None;
                    self.error = None;
                    self.phase = Phase::UpToDate;
                }
            }
            Err(message) => {
                if self.held.is_some() {
                    self.phase = Phase::Ready;
                } else if self.latest.is_some() {
                    // A failed check never hides a found update.
                    self.phase = Phase::Available;
                } else {
                    self.error = Some(message);
                    self.phase = Phase::Failed;
                }
            }
        }
    }

    fn poll_download(&mut self) -> bool {
        let Some(rx) = self.download_rx.as_ref() else {
            return false;
        };
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(DownloadEvent::Progress(received)) => {
                    let total = self
                        .latest
                        .as_ref()
                        .and_then(|c| c.package.as_ref())
                        .map_or(1, |p| p.size);
                    if matches!(self.phase, Phase::Downloading { .. }) {
                        self.phase = Phase::Downloading {
                            percent: percent(received, total),
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::Done(result)) => {
                    self.download_rx = None;
                    self.finish_download(result);
                    return true;
                }
                Err(TryRecvError::Empty) => return changed,
                Err(TryRecvError::Disconnected) => {
                    self.download_rx = None;
                    self.finish_download(Err("The download stopped unexpectedly.".into()));
                    return true;
                }
            }
        }
    }

    fn finish_download(&mut self, result: Result<Downloaded, String>) {
        let release = self
            .latest
            .as_ref()
            .and_then(|c| c.package.clone().map(|p| (c.version.clone(), p)));
        match (result, release) {
            (Ok(done), Some((version, package))) => {
                self.held = Some(Held {
                    version,
                    dir: done.dir,
                    path: done.path,
                    file: done.file,
                    package,
                    sha256: done.sha256,
                });
                self.error = None;
                self.phase = Phase::Ready;
            }
            (Ok(done), None) => {
                drop(done.file);
                remove_folder(&done.dir);
                self.fail_download("The download finished without a release.".into());
            }
            (Err(message), _) => self.fail_download(message),
        }
    }

    /// A failed download returns to Available with a retry.
    fn fail_download(&mut self, message: String) {
        self.error = Some(message);
        self.phase = if self.latest.is_some() {
            Phase::Available
        } else {
            Phase::Failed
        };
    }

    fn poll_install(&mut self) -> bool {
        let Some(rx) = self.install_rx.as_ref() else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(InstallFailure::Failed(
                "The installer stopped unexpectedly.".into(),
            )),
        };
        self.install_rx = None;
        match result {
            Ok(()) => {
                if let Some(held) = self.held.take() {
                    discard(held);
                }
                self.error = None;
                self.phase = Phase::Installed;
            }
            Err(InstallFailure::Rejected(message)) => {
                if let Some(held) = self.held.take() {
                    discard(held);
                }
                self.error = Some(format!("{message} Download it again."));
                self.phase = Phase::Available;
            }
            Err(InstallFailure::Failed(message)) => {
                self.error = Some(message);
                self.phase = Phase::Ready;
            }
        }
        true
    }
}

impl Drop for Updates {
    /// A package that was never installed is removed with its private folder.
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            discard(held);
        }
    }
}

fn discard(held: Held) {
    let Held { dir, file, .. } = held;
    drop(file);
    remove_folder(&dir);
}

fn percent(received: u64, total: u64) -> u64 {
    (received.saturating_mul(100) / total.max(1)).min(100)
}

// ----------------------------------------------------------- install kind

fn ineligible(kind: InstallKind) -> Option<&'static str> {
    (kind == InstallKind::Unmanaged).then_some(
        "This copy of Wavelane was not installed from a package; download updates from GitHub.",
    )
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// A development build run from the source tree (`target/…`): packages
/// install `/usr/bin/wavelane`, the tarball `~/.local/bin/wavelane`.
pub fn dev_build() -> bool {
    cfg!(feature = "dev-shortcut")
        || std::env::current_exe().is_ok_and(|exe| exe.components().any(|c| c.as_os_str() == "target"))
}

fn detect_kind() -> InstallKind {
    let exe = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .ok();
    let home = home_dir().and_then(|home| home.canonicalize().ok());
    classify_kind(exe.as_deref(), home.as_deref(), &|program, flag| {
        Command::new(program)
            .args([flag, "/usr/bin/wavelane"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// `owned(program, flag)` runs `rpm -qf` / `dpkg -S` / `pacman -Qo` on
/// `/usr/bin/wavelane`.
fn classify_kind(
    exe: Option<&Path>,
    home: Option<&Path>,
    owned: &dyn Fn(&str, &str) -> bool,
) -> InstallKind {
    let Some(exe) = exe else {
        return InstallKind::Unmanaged;
    };
    if exe == Path::new("/usr/bin/wavelane") {
        if owned("rpm", "-qf") {
            InstallKind::Rpm
        } else if owned("dpkg", "-S") {
            InstallKind::Deb
        } else if owned("pacman", "-Qo") {
            InstallKind::Pacman
        } else {
            InstallKind::Unmanaged
        }
    } else if home.is_some_and(|home| exe == home.join(".local/bin/wavelane")) {
        InstallKind::Tarball
    } else {
        InstallKind::Unmanaged
    }
}

fn launch_target(kind: InstallKind, home: Option<&Path>) -> Option<PathBuf> {
    match kind {
        InstallKind::Rpm | InstallKind::Deb | InstallKind::Pacman => Some(PathBuf::from("/usr/bin/wavelane")),
        InstallKind::Tarball => home.map(|home| home.join(".local/bin/wavelane")),
        InstallKind::Unmanaged => None,
    }
}

fn spawn_relaunch(
    pid: u32,
    exe: &Path,
    args: &[String],
    activation_token: Option<&str>,
) -> Result<(), String> {
    const SCRIPT: &str = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; shift; exec "$@""#;
    let build = |wrapper: Option<&str>| {
        let mut command = match wrapper {
            Some(wrapper) => {
                let mut command = Command::new(wrapper);
                command.arg("/bin/sh");
                command
            }
            None => Command::new("/bin/sh"),
        };
        command
            .args(["-c", SCRIPT, "sh"])
            .arg(pid.to_string())
            .arg(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(token) = activation_token {
            command.env("XDG_ACTIVATION_TOKEN", token);
        }
        command
    };
    match build(Some("setsid")).spawn() {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::process::CommandExt;
            build(None)
                .process_group(0)
                .spawn()
                .map(|_| ())
                .map_err(|_| "Wavelane could not be restarted.".to_owned())
        }
        Err(_) => Err("Wavelane could not be restarted.".into()),
    }
}

// ------------------------------------------------------------------- mode

fn xdg_dir(variable: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home_dir().map(|home| home.join(fallback)))
}

fn config_path() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|base| base.join("wavelane/updates"))
}

fn cache_root() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|base| base.join("wavelane"))
}

const MODE_NOTIFY: &[u8] = b"wavelane-updates=1\nmode=notify\n";
const MODE_OFF: &[u8] = b"wavelane-updates=1\nmode=off\n";

/// Missing or unrecognised content is Notify; the file is left untouched.
fn read_mode(path: &Path) -> UpdateMode {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= 4096 => {}
        _ => return UpdateMode::Notify,
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes == MODE_OFF => UpdateMode::Off,
        _ => UpdateMode::Notify,
    }
}

fn write_mode(path: &Path, mode: UpdateMode) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("No Wavelane settings directory.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let data = match mode {
        UpdateMode::Notify => MODE_NOTIFY,
        UpdateMode::Off => MODE_OFF,
    };
    let temp = parent.join(format!(
        ".updates-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

// ------------------------------------------------------------- feed parse

/// Release notes: control characters and markdown emphasis removed, at most
/// 64 KiB read, 80 lines of 400 characters.
fn clean_notes(body: &str) -> Vec<String> {
    let mut end = body.len().min(MAX_NOTES_BYTES);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut notes: Vec<String> = Vec::new();
    for raw in body[..end].lines() {
        let text = clean_line(raw);
        if text.is_empty() && notes.last().is_none_or(String::is_empty) {
            continue;
        }
        notes.push(text);
        if notes.len() == MAX_NOTE_LINES {
            break;
        }
    }
    while notes.last().is_some_and(String::is_empty) {
        notes.pop();
    }
    notes
}

fn clean_line(raw: &str) -> String {
    let text: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '`')
        .collect::<String>()
        .replace("**", "");
    let text = text.trim();
    let text = if text.starts_with('#') {
        text.trim_start_matches('#').trim_start()
    } else if let Some(item) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
        return format!("\u{2022} {}", item.trim())
            .chars()
            .take(MAX_NOTE_CHARS)
            .collect();
    } else {
        text
    };
    text.chars().take(MAX_NOTE_CHARS).collect()
}

fn digits(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_digit())
}

/// A release version as each package format spells it: a SemVer
/// pre-release (`0.2.0-preview.3`) is `0.2.0~preview.3` for rpm and deb
/// (sorts before 0.2.0) and `0.2.0preview.3` for pacman (no `-` allowed).
fn package_version(kind: InstallKind, version: &str) -> String {
    match kind {
        InstallKind::Rpm | InstallKind::Deb => version.replacen('-', "~", 1),
        InstallKind::Pacman => version.replacen('-', "", 1),
        InstallKind::Tarball | InstallKind::Unmanaged => version.to_owned(),
    }
}

/// Exact package file name for this install kind and version.
fn package_name_ok(kind: InstallKind, version: &str, name: &str) -> bool {
    let version = &package_version(kind, version);
    match kind {
        InstallKind::Rpm => name
            .strip_prefix(&format!("wavelane-{version}-"))
            .and_then(|rest| rest.strip_suffix(".x86_64.rpm"))
            .and_then(|rest| rest.split_once(".fc"))
            .is_some_and(|(release, fedora)| digits(release, 4) && digits(fedora, 3)),
        InstallKind::Deb => name
            .strip_prefix(&format!("wavelane_{version}-"))
            .and_then(|rest| rest.strip_suffix("_amd64.deb"))
            .is_some_and(|release| digits(release, 4)),
        InstallKind::Pacman => name
            .strip_prefix(&format!("wavelane-{version}-"))
            .and_then(|rest| rest.strip_suffix("-x86_64.pkg.tar.zst"))
            .is_some_and(|release| digits(release, 4)),
        InstallKind::Tarball => name == format!("wavelane-{version}-x86_64-linux.tar.gz"),
        InstallKind::Unmanaged => false,
    }
}

/// The package for `kind` among the release's assets, only when `SHA256SUMS`
/// is published beside it.
fn select_package(kind: InstallKind, version: &str, assets: &[(String, u64)]) -> Option<Package> {
    assets.iter().find(|(name, _)| name == SUMS_NAME)?;
    assets
        .iter()
        .find(|(name, size)| {
            package_name_ok(kind, version, name) && *size > 0 && *size <= MAX_PACKAGE_BYTES
        })
        .map(|(name, size)| Package {
            name: name.clone(),
            size: *size,
        })
}

/// The newest published release newer than `current`. Drafts and tags that
/// are not `v<SemVer>` are ignored; pre-releases count.
fn parse_releases(
    body: &[u8],
    kind: InstallKind,
    current: &str,
) -> Result<Option<Candidate>, String> {
    let current = Version::parse(current)
        .map_err(|_| "This build has an invalid version string.".to_owned())?;
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| "The update response could not be read.".to_owned())?;
    let list = value
        .as_array()
        .ok_or("The update response was not a release list.")?;
    let mut best: Option<(Version, &Value)> = None;
    for release in list {
        if release.get("draft").and_then(Value::as_bool) != Some(false) {
            continue;
        }
        let Some(tag) = release.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(text) = tag
            .strip_prefix('v')
            .filter(|t| t.len() <= MAX_VERSION_LENGTH)
        else {
            continue;
        };
        let Ok(version) = Version::parse(text) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(top, _)| version.cmp_precedence(top).is_gt())
        {
            best = Some((version, release));
        }
    }
    let Some((version, release)) = best else {
        return Ok(None);
    };
    if !version.cmp_precedence(&current).is_gt() {
        return Ok(None);
    }
    let version = version.to_string();
    let assets: Vec<(String, u64)> = release
        .get("assets")
        .and_then(Value::as_array)
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| {
                    Some((
                        asset.get("name")?.as_str()?.to_owned(),
                        asset.get("size")?.as_u64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Candidate {
        notes: clean_notes(
            release
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        package: select_package(kind, &version, &assets),
        version,
    }))
}

/// The SHA-256 listed for `name` in a `SHA256SUMS` file
/// (`<64 lowercase hex>  <name>`); conflicting entries are rejected.
fn parse_sums(text: &str, name: &str) -> Option<String> {
    let mut found: Option<&str> = None;
    for line in text.lines() {
        let Some((hash, file)) = line.split_once("  ") else {
            continue;
        };
        if file != name {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        if found.is_some_and(|previous| previous != hash) {
            return None;
        }
        found = Some(hash);
    }
    found.map(str::to_owned)
}

fn package_url(version: &str, name: &str) -> String {
    format!("{DOWNLOAD_BASE}/v{version}/{name}")
}

/// A validator is echoed back in a request header, so it must be short,
/// printable ASCII.
fn safe_validator(value: Option<String>) -> Option<String> {
    value.filter(|text| {
        !text.is_empty()
            && text.len() <= MAX_VALIDATOR_LENGTH
            && text.bytes().all(|byte| (0x20..0x7f).contains(&byte))
    })
}

/// Interpret one feed response: 200 parses (and caches with its `ETag`), 304
/// reuses the cache, 404 means nothing is published.
fn interpret_feed(
    status: u16,
    body: Vec<u8>,
    etag: Option<String>,
    cache: Option<FeedCache>,
    kind: InstallKind,
    current: &str,
) -> (Result<Option<Candidate>, String>, Option<FeedCache>) {
    match status {
        200 => {
            if body.len() > MAX_FEED_BYTES {
                return (Err("The update response was too large.".into()), None);
            }
            let result = parse_releases(&body, kind, current);
            let cache = safe_validator(etag)
                .filter(|_| result.is_ok())
                .map(|etag| FeedCache { etag, body });
            (result, cache)
        }
        304 => match cache {
            Some(cache) => (parse_releases(&cache.body, kind, current), None),
            None => (
                Err("The update service answered 304 without a cached list.".into()),
                None,
            ),
        },
        404 => (Ok(None), None),
        403 | 429 => (
            Err("GitHub is limiting update checks right now; try again later.".into()),
            None,
        ),
        status => (
            Err(format!("The update service answered HTTP {status}.")),
            None,
        ),
    }
}

// ---------------------------------------------------------------- network

/// Redirects stay on GitHub's own https hosts.
fn redirect_allowed(url: &reqwest::Url, hops: usize) -> bool {
    hops <= MAX_REDIRECTS
        && url.scheme() == "https"
        && url.port().is_none_or(|port| port == 443)
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| ALLOWED_HOSTS.contains(&host))
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if redirect_allowed(attempt.url(), attempt.previous().len()) {
                attempt.follow()
            } else {
                attempt.error("redirect not allowed")
            }
        }))
        .tls_built_in_webpki_certs(false)
        .tls_built_in_native_certs(true)
        .https_only(true)
        .user_agent(concat!("Wavelane/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Could not initialize update networking.".into())
}

fn get(
    timeout: Duration,
    url: &str,
    headers: &[(&'static str, String)],
) -> Result<reqwest::blocking::Response, String> {
    let mut request = client(timeout)?.get(url);
    for (name, value) in headers {
        request = request.header(*name, value.as_str());
    }
    request.send().map_err(|error| {
        if error.is_timeout() {
            "The update service did not respond in time.".to_owned()
        } else {
            "Could not reach the update service.".to_owned()
        }
    })
}

/// Read at most `max` bytes; more is an error.
fn read_bounded(source: impl Read, max: usize, message: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(4096);
    source
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The update response could not be read.".to_owned())?;
    if bytes.len() > max {
        return Err(message.to_owned());
    }
    Ok(bytes)
}

fn fetch_release(cache: Option<FeedCache>, kind: InstallKind) -> CheckOutcome {
    let mut headers = vec![
        ("Accept", "application/vnd.github+json".to_owned()),
        ("X-GitHub-Api-Version", "2022-11-28".to_owned()),
    ];
    if let Some(cache) = &cache {
        headers.push(("If-None-Match", cache.etag.clone()));
    }
    let fetched = get(CHECK_TIMEOUT, FEED_URL, &headers).and_then(|response| {
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = if status == 200 {
            read_bounded(
                response,
                MAX_FEED_BYTES,
                "The update response was too large.",
            )?
        } else {
            Vec::new()
        };
        Ok((status, body, etag))
    });
    match fetched {
        Ok((status, body, etag)) => {
            let (result, cache) = interpret_feed(status, body, etag, cache, kind, CURRENT_VERSION);
            CheckOutcome { result, cache }
        }
        Err(error) => CheckOutcome {
            result: Err(error),
            cache: None,
        },
    }
}

fn fetch_sums(version: &str, name: &str) -> Result<String, String> {
    let response = get(CHECK_TIMEOUT, &package_url(version, SUMS_NAME), &[])?;
    if response.status().as_u16() != 200 {
        return Err("The package checksums are not published for this release.".into());
    }
    let body = read_bounded(response, MAX_SUMS_BYTES, "The checksum list was too large.")?;
    std::str::from_utf8(&body)
        .ok()
        .and_then(|text| parse_sums(text, name))
        .ok_or_else(|| "The package has no valid published SHA-256.".to_owned())
}

// --------------------------------------------------------------- download

fn private_dir(root: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = root.join(format!(
        "{FOLDER_PREFIX}{}-{:x}-{serial}",
        std::process::id(),
        nanos & 0xffff_ffff_ffff
    ));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(root)
        .and_then(|()| {
            let mut leaf = std::fs::DirBuilder::new();
            leaf.mode(0o700);
            leaf.create(&directory)
        })
        .map_err(|_| "The update could not be saved to the cache folder.".to_owned())?;
    Ok(directory)
}

/// Remove one of our private `update-*` folders (never anything else).
fn remove_folder(directory: &Path) {
    if directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(FOLDER_PREFIX))
    {
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// Delete download folders left by earlier runs (not `keep`, not another
/// running Wavelane's).
fn discard_old_downloads(root: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(rest) = name.strip_prefix(FOLDER_PREFIX) else {
            continue;
        };
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        let other_running = rest
            .split('-')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| {
                pid != std::process::id() && Path::new(&format!("/proc/{pid}")).exists()
            });
        if is_dir && !other_running && keep != Some(path.as_path()) {
            remove_folder(&path);
        }
    }
}

fn download(
    root: &Path,
    version: &str,
    package: &Package,
    progress: &mut dyn FnMut(u64),
) -> Result<Downloaded, String> {
    let sha256 = fetch_sums(version, &package.name)?;
    discard_old_downloads(root, None);
    let response = get(DOWNLOAD_TIMEOUT, &package_url(version, &package.name), &[])?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err("The package for this release is not published.".into()),
        status => return Err(format!("The download failed (HTTP {status}).")),
    }
    save_package(response, root, package, &sha256, progress)
}

/// Stream `source` into a fresh private folder under `root`, enforcing size
/// and SHA-256 as it arrives, then reopen the file and verify it again. Any
/// failure deletes the folder.
fn save_package(
    source: impl Read,
    root: &Path,
    package: &Package,
    sha256: &str,
    progress: &mut dyn FnMut(u64),
) -> Result<Downloaded, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let directory = private_dir(root)?;
    let partial = directory.join(format!("{}.partial", package.name));
    let target = directory.join(&package.name);
    let saved = "The update could not be saved to the cache folder.";
    let result = (|| {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|_| saved.to_owned())?;
        let mut source = source.take(package.size.min(MAX_PACKAGE_BYTES) + 1);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|_| "The download was interrupted or timed out.".to_owned())?;
            if read == 0 {
                break;
            }
            received += read as u64;
            if received > package.size {
                return Err(
                    "The download was larger than the published size and was discarded.".into(),
                );
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .map_err(|_| "The update could not be written to disk.".to_owned())?;
            progress(received);
        }
        output
            .sync_all()
            .map_err(|_| "The update could not be written to disk.".to_owned())?;
        drop(output);
        if received != package.size {
            return Err(format!(
                "The download was incomplete ({received} of {} bytes).",
                package.size
            ));
        }
        if hex(&hasher.finalize()) != sha256 {
            return Err(
                "The download did not match the published SHA-256 and was discarded.".into(),
            );
        }
        std::fs::rename(&partial, &target).map_err(|_| saved.to_owned())?;
        let held =
            File::open(&target).map_err(|_| "The download could not be reopened.".to_owned())?;
        verify_file(&held, package.size, sha256)?;
        Ok(held)
    })();
    match result {
        Ok(file) => Ok(Downloaded {
            dir: directory,
            path: target,
            file,
            sha256: sha256.to_owned(),
        }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&directory);
            Err(error)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_file(file: &File, size: u64, sha256: &str) -> Result<(), String> {
    use std::io::{Seek, SeekFrom};
    let changed =
        || "The downloaded update changed after verification and was not installed.".to_owned();
    let mut file = file;
    file.seek(SeekFrom::Start(0)).map_err(|_| changed())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|_| changed())?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > size {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if total != size || hex(&hasher.finalize()) != sha256 {
        return Err(changed());
    }
    Ok(())
}

// ---------------------------------------------------------------- install

struct InstallJob {
    kind: InstallKind,
    version: String,
    path: PathBuf,
    file: File,
    size: u64,
    sha256: String,
    work_root: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl InstallJob {
    fn run(self) -> Result<(), InstallFailure> {
        verify_file(&self.file, self.size, &self.sha256).map_err(InstallFailure::Rejected)?;
        let failed = InstallFailure::Failed;
        match self.kind {
            InstallKind::Rpm => {
                run_privileged(&["/usr/bin/dnf", "install", "-y"], &self.path).map_err(failed)
            }
            InstallKind::Deb => {
                run_privileged(&["/usr/bin/apt-get", "install", "-y"], &self.path).map_err(failed)
            }
            InstallKind::Pacman => {
                run_privileged(&["/usr/bin/pacman", "-U", "--noconfirm"], &self.path).map_err(failed)
            }
            InstallKind::Tarball => {
                let home = self
                    .home
                    .as_deref()
                    .ok_or_else(|| failed("There is no home folder to install into.".into()))?;
                let root = self
                    .work_root
                    .as_deref()
                    .ok_or_else(|| failed("There is no working folder.".into()))?;
                install_tarball(&self.path, root, home, &self.version).map_err(failed)
            }
            InstallKind::Unmanaged => Err(failed(
                ineligible(InstallKind::Unmanaged)
                    .unwrap_or_default()
                    .into(),
            )),
        }
    }
}

/// `pkexec <command> <package>`; the system asks for the password.
fn run_privileged(command: &[&str], package: &Path) -> Result<(), String> {
    if !package.is_absolute() {
        return Err("The package path is not absolute.".into());
    }
    let output = Command::new("pkexec")
        .args(command)
        .arg(package)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "Could not start the system installer (pkexec).".to_owned())?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(126 | 127) => Err("Authentication was cancelled.".into()),
        code => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .rev()
                .map(clean_line)
                .find(|line| !line.is_empty())
                .unwrap_or_default();
            let code = code.map_or_else(|| "a signal".to_owned(), |code| format!("exit {code}"));
            Err(if detail.is_empty() {
                format!("The installer failed ({code}).")
            } else {
                format!("The installer failed ({code}): {detail}")
            })
        }
    }
}

/// A regular file at `root`/`parts`, none of whose components is a symlink.
fn regular_file(root: &Path, parts: &[&str], max: u64) -> Result<PathBuf, String> {
    let bad = || format!("The update archive is missing {}.", parts.join("/"));
    let mut path = root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| bad())?;
        let last = index + 1 == parts.len();
        let fine = if last { meta.is_file() } else { meta.is_dir() };
        if !fine || meta.file_type().is_symlink() {
            return Err(bad());
        }
        if last && meta.len() > max {
            return Err(bad());
        }
    }
    Ok(path)
}

/// Copy `source` to a temp file in `destination`'s folder, then rename it in.
fn install_file(source: &Path, destination: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let message = format!("Could not install {}.", destination.display());
    let failed = |_: std::io::Error| message.clone();
    let parent = destination.parent().ok_or_else(|| message.clone())?;
    std::fs::create_dir_all(parent).map_err(failed)?;
    let temp = parent.join(format!(
        ".wavelane-update-{}-{}",
        std::process::id(),
        destination
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
    ));
    let _ = std::fs::remove_file(&temp);
    let result = (|| {
        let mut input = File::open(source)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temp, destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(failed)
}

/// Extract the tarball in a private folder under `work_root` and install its
/// binary, desktop entry and icon into `home`/.local. Nothing else is used.
fn install_tarball(
    archive: &Path,
    work_root: &Path,
    home: &Path,
    version: &str,
) -> Result<(), String> {
    let scratch = private_dir(work_root)?;
    let result = (|| {
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(&scratch)
            .args(["--no-same-owner", "--no-same-permissions"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "Could not start tar to unpack the update.".to_owned())?;
        if !status.success() {
            return Err("The update archive could not be unpacked.".into());
        }
        let top = format!("wavelane-{version}-x86_64-linux");
        let find = |parts: &[&str], max| {
            let mut full = vec![top.as_str()];
            full.extend_from_slice(parts);
            regular_file(&scratch, &full, max)
        };
        let binary = find(&TAR_BIN, MAX_PACKAGE_BYTES)?;
        let desktop = find(&TAR_DESKTOP, MAX_SMALL_FILE_BYTES)?;
        let mut icon_parts = TAR_ICON.to_vec();
        icon_parts.push(ICON_NAME);
        let icon = find(&icon_parts, MAX_SMALL_FILE_BYTES)?;
        let local = home.join(".local");
        install_file(
            &icon,
            &local
                .join("share/icons/hicolor/scalable/apps")
                .join(ICON_NAME),
            0o644,
        )?;
        install_file(
            &desktop,
            &local.join("share/applications/digital.zombie.wavelane.desktop"),
            0o644,
        )?;
        install_file(&binary, &local.join("bin/wavelane"), 0o755)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wavelane-updates-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assets(names: &[&str]) -> Vec<(String, u64)> {
        names.iter().map(|n| ((*n).to_owned(), 1000)).collect()
    }

    fn feed() -> String {
        serde_json::json!([
            {"tag_name": "v0.1.3", "draft": false, "prerelease": true, "body": "old", "assets": []},
            {"tag_name": "v0.9.0", "draft": true, "prerelease": false, "body": "draft", "assets": []},
            {"tag_name": "v0.2.0-rc.1", "draft": false, "prerelease": true, "body": "rc", "assets": []},
            {"tag_name": "v0.2.0", "draft": false, "prerelease": true,
             "body": "## New\n- **Fast** `thing`\n\n\n- more",
             "assets": [
               {"name": "SHA256SUMS", "size": 300, "browser_download_url": "https://evil.example/x"},
               {"name": "wavelane-0.2.0-1.fc44.x86_64.rpm", "size": 5000},
               {"name": "wavelane_0.2.0-1_amd64.deb", "size": 6000},
               {"name": "wavelane-0.2.0-x86_64-linux.tar.gz", "size": 7000}
             ]},
            {"tag_name": "v1.0", "draft": false, "body": "bad semver", "assets": []},
            {"tag_name": "1.5.0", "draft": false, "body": "no v", "assets": []}
        ])
        .to_string()
    }

    #[test]
    fn picks_highest_published_semver() {
        let c = parse_releases(feed().as_bytes(), InstallKind::Rpm, "0.1.3")
            .unwrap()
            .unwrap();
        assert_eq!(c.version, "0.2.0");
        assert_eq!(
            c.notes,
            vec!["New", "\u{2022} Fast thing", "", "\u{2022} more"]
        );
        let package = c.package.unwrap();
        assert_eq!(package.name, "wavelane-0.2.0-1.fc44.x86_64.rpm");
        assert_eq!(package.size, 5000);
    }

    #[test]
    fn package_per_kind() {
        let deb = parse_releases(feed().as_bytes(), InstallKind::Deb, "0.1.3")
            .unwrap()
            .unwrap();
        assert_eq!(deb.package.unwrap().name, "wavelane_0.2.0-1_amd64.deb");
        let tar = parse_releases(feed().as_bytes(), InstallKind::Tarball, "0.1.3")
            .unwrap()
            .unwrap();
        assert_eq!(tar.package.unwrap().size, 7000);
        let none = parse_releases(feed().as_bytes(), InstallKind::Unmanaged, "0.1.3")
            .unwrap()
            .unwrap();
        assert!(none.package.is_none());
    }

    #[test]
    fn only_newer_counts() {
        assert_eq!(
            parse_releases(feed().as_bytes(), InstallKind::Rpm, "0.2.0").unwrap(),
            None
        );
        assert_eq!(
            parse_releases(feed().as_bytes(), InstallKind::Rpm, "0.3.0").unwrap(),
            None
        );
        assert_eq!(
            parse_releases(b"[]", InstallKind::Rpm, "0.1.3").unwrap(),
            None
        );
        assert!(parse_releases(b"{}", InstallKind::Rpm, "0.1.3").is_err());
        assert!(parse_releases(b"nope", InstallKind::Rpm, "0.1.3").is_err());
    }

    #[test]
    fn prerelease_ordering() {
        let body = serde_json::json!([
            {"tag_name": "v0.2.0-rc.2", "draft": false},
            {"tag_name": "v0.2.0-rc.10", "draft": false},
            {"tag_name": "v0.2.0-beta.1", "draft": false}
        ])
        .to_string();
        let c = parse_releases(body.as_bytes(), InstallKind::Rpm, "0.1.0")
            .unwrap()
            .unwrap();
        assert_eq!(c.version, "0.2.0-rc.10");
    }

    #[test]
    fn asset_names_are_exact() {
        let v = "0.2.0";
        assert!(package_name_ok(
            InstallKind::Rpm,
            v,
            "wavelane-0.2.0-1.fc44.x86_64.rpm"
        ));
        assert!(package_name_ok(
            InstallKind::Rpm,
            v,
            "wavelane-0.2.0-12.fc100.x86_64.rpm"
        ));
        for bad in [
            "wavelane-0.2.0-1.x86_64.rpm",
            "wavelane-0.2.0-.fc44.x86_64.rpm",
            "wavelane-0.2.0-1.fc.x86_64.rpm",
            "wavelane-0.2.0-1.fc44.x86_64.rpm.sig",
            "../wavelane-0.2.0-1.fc44.x86_64.rpm",
            "wavelane-0.2.1-1.fc44.x86_64.rpm",
            "wavelane-0.2.0-1x.fc44.x86_64.rpm",
            "wavelane-0.2.0-1.fc44.aarch64.rpm",
        ] {
            assert!(!package_name_ok(InstallKind::Rpm, v, bad), "{bad}");
        }
        assert!(package_name_ok(
            InstallKind::Deb,
            v,
            "wavelane_0.2.0-1_amd64.deb"
        ));
        assert!(!package_name_ok(
            InstallKind::Deb,
            v,
            "wavelane_0.2.0-1_arm64.deb"
        ));
        assert!(!package_name_ok(
            InstallKind::Deb,
            v,
            "wavelane_0.2.0_amd64.deb"
        ));
        assert!(!package_name_ok(
            InstallKind::Deb,
            v,
            "wavelane_0.2.0-1/x_amd64.deb"
        ));
        assert!(package_name_ok(
            InstallKind::Tarball,
            v,
            "wavelane-0.2.0-x86_64-linux.tar.gz"
        ));
        assert!(!package_name_ok(
            InstallKind::Tarball,
            v,
            "wavelane-0.2.0-x86_64-linux.tar.gz2"
        ));
        assert!(!package_name_ok(
            InstallKind::Unmanaged,
            v,
            "wavelane-0.2.0-x86_64-linux.tar.gz"
        ));
    }

    #[test]
    fn package_needs_sums_and_sane_size() {
        let v = "0.2.0";
        let name = "wavelane-0.2.0-x86_64-linux.tar.gz";
        assert!(select_package(InstallKind::Tarball, v, &assets(&[name])).is_none());
        assert!(select_package(InstallKind::Tarball, v, &assets(&[name, "SHA256SUMS"])).is_some());
        let big = vec![
            (name.to_owned(), MAX_PACKAGE_BYTES + 1),
            ("SHA256SUMS".to_owned(), 10),
        ];
        assert!(select_package(InstallKind::Tarball, v, &big).is_none());
        let empty = vec![(name.to_owned(), 0), ("SHA256SUMS".to_owned(), 10)];
        assert!(select_package(InstallKind::Tarball, v, &empty).is_none());
    }

    #[test]
    fn sums_parsing() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = format!("{a}  one.rpm\n{b}  two.deb\n");
        assert_eq!(parse_sums(&text, "one.rpm"), Some(a.clone()));
        assert_eq!(parse_sums(&text, "two.deb"), Some(b.clone()));
        assert_eq!(parse_sums(&text, "three"), None);
        assert_eq!(parse_sums(&text, "one"), None);
        assert_eq!(parse_sums(&format!("{}  x\n", "A".repeat(64)), "x"), None);
        assert_eq!(parse_sums(&format!("{}  x\n", "a".repeat(63)), "x"), None);
        assert_eq!(parse_sums(&format!("{a} x\n"), "x"), None);
        assert_eq!(parse_sums(&format!("{a}  x\n{b}  x\n"), "x"), None);
        assert_eq!(parse_sums(&format!("{a}  x\n{a}  x\n"), "x"), Some(a));
    }

    #[test]
    fn urls_are_derived() {
        assert_eq!(
            package_url("0.2.0", "wavelane-0.2.0-x86_64-linux.tar.gz"),
            "https://github.com/joenilan/wavelane/releases/download/v0.2.0/wavelane-0.2.0-x86_64-linux.tar.gz"
        );
    }

    #[test]
    fn redirect_allowlist() {
        let ok = |url: &str, hops| redirect_allowed(&reqwest::Url::parse(url).unwrap(), hops);
        assert!(ok("https://github.com/a/b", 1));
        assert!(ok("https://api.github.com/x", 1));
        assert!(ok("https://objects.githubusercontent.com/x", 2));
        assert!(ok("https://release-assets.githubusercontent.com/x", 5));
        assert!(ok("https://github.com:443/x", 1));
        assert!(!ok("https://github.com/x", 6));
        assert!(!ok("http://github.com/x", 1));
        assert!(!ok("https://evil.example/x", 1));
        assert!(!ok("https://github.com.evil.example/x", 1));
        assert!(!ok("https://raw.githubusercontent.com/x", 1));
        assert!(!ok("https://user@github.com/x", 1));
        assert!(!ok("https://github.com:8443/x", 1));
    }

    #[test]
    fn feed_responses() {
        let kind = InstallKind::Tarball;
        let (r, cache) = interpret_feed(
            200,
            feed().into_bytes(),
            Some("\"abc\"".into()),
            None,
            kind,
            "0.1.3",
        );
        assert_eq!(r.unwrap().unwrap().version, "0.2.0");
        let cache = cache.unwrap();
        assert_eq!(cache.etag, "\"abc\"");
        let (r, none) = interpret_feed(304, vec![], None, Some(cache), kind, "0.1.3");
        assert_eq!(r.unwrap().unwrap().version, "0.2.0");
        assert!(none.is_none());
        assert!(
            interpret_feed(304, vec![], None, None, kind, "0.1.3")
                .0
                .is_err()
        );
        assert_eq!(
            interpret_feed(404, vec![], None, None, kind, "0.1.3").0,
            Ok(None)
        );
        assert!(
            interpret_feed(500, vec![], None, None, kind, "0.1.3")
                .0
                .is_err()
        );
        let (r, cache) = interpret_feed(
            200,
            feed().into_bytes(),
            Some("bad\nvalue".into()),
            None,
            kind,
            "0.1.3",
        );
        assert!(r.is_ok() && cache.is_none());
        let (r, _) = interpret_feed(
            200,
            vec![b' '; MAX_FEED_BYTES + 1],
            None,
            None,
            kind,
            "0.1.3",
        );
        assert!(r.is_err());
    }

    #[test]
    fn note_cleaning() {
        let long = "x".repeat(1000);
        let body = format!("\n\n# Title\r\nLine `code` with **bold**\x07\n* star\n{long}\n");
        let notes = clean_notes(&body);
        assert_eq!(notes[0], "Title");
        assert_eq!(notes[1], "Line code with bold");
        assert_eq!(notes[2], "\u{2022} star");
        assert_eq!(notes[3].chars().count(), MAX_NOTE_CHARS);
        let many = "a\n".repeat(500);
        assert_eq!(clean_notes(&many).len(), MAX_NOTE_LINES);
        let huge = "é".repeat(MAX_NOTES_BYTES);
        assert!(!clean_notes(&huge).is_empty());
        assert!(clean_notes("  \n\n").is_empty());
    }

    #[test]
    fn mode_file_round_trip() {
        let dir = temp("mode");
        let path = dir.join("wavelane/updates");
        assert_eq!(read_mode(&path), UpdateMode::Notify);
        write_mode(&path, UpdateMode::Off).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"wavelane-updates=1\nmode=off\n");
        assert_eq!(read_mode(&path), UpdateMode::Off);
        write_mode(&path, UpdateMode::Notify).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"wavelane-updates=1\nmode=notify\n"
        );
        assert_eq!(read_mode(&path), UpdateMode::Notify);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, "garbage mode=off").unwrap();
        assert_eq!(read_mode(&path), UpdateMode::Notify);
        assert_eq!(std::fs::read(&path).unwrap(), b"garbage mode=off");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn install_kind_detection() {
        let home = Path::new("/home/me");
        let rpm = |p: &str, _: &str| p == "rpm";
        let dpkg = |p: &str, _: &str| p == "dpkg";
        let none = |_: &str, _: &str| false;
        let usr = Some(Path::new("/usr/bin/wavelane"));
        assert_eq!(classify_kind(usr, Some(home), &rpm), InstallKind::Rpm);
        assert_eq!(classify_kind(usr, Some(home), &dpkg), InstallKind::Deb);
        assert_eq!(
            classify_kind(usr, Some(home), &none),
            InstallKind::Unmanaged
        );
        let local = Some(Path::new("/home/me/.local/bin/wavelane"));
        assert_eq!(classify_kind(local, Some(home), &rpm), InstallKind::Tarball);
        assert_eq!(classify_kind(local, None, &none), InstallKind::Unmanaged);
        let build = Some(Path::new("/home/me/git/target/release/wavelane"));
        assert_eq!(
            classify_kind(build, Some(home), &rpm),
            InstallKind::Unmanaged
        );
        assert_eq!(
            classify_kind(None, Some(home), &rpm),
            InstallKind::Unmanaged
        );
        assert!(ineligible(InstallKind::Unmanaged).is_some());
        assert!(ineligible(InstallKind::Rpm).is_none());
        assert_eq!(
            launch_target(InstallKind::Rpm, None),
            Some(PathBuf::from("/usr/bin/wavelane"))
        );
        assert_eq!(
            launch_target(InstallKind::Tarball, Some(home)),
            Some(PathBuf::from("/home/me/.local/bin/wavelane"))
        );
        assert_eq!(launch_target(InstallKind::Unmanaged, Some(home)), None);
    }

    fn package_of(bytes: &[u8]) -> (Package, String) {
        (
            Package {
                name: "p.bin".into(),
                size: bytes.len() as u64,
            },
            hex(&Sha256::digest(bytes)),
        )
    }

    #[test]
    fn download_verifies_size_and_hash() {
        let root = temp("download");
        let data = vec![7u8; 200_000];
        let (package, sha) = package_of(&data);
        let mut seen = 0;
        let done = save_package(Cursor::new(data.clone()), &root, &package, &sha, &mut |n| {
            seen = n
        })
        .unwrap();
        assert_eq!(seen, 200_000);
        assert!(done.path.is_file());
        verify_file(&done.file, package.size, &sha).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&done.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // A changed file fails the re-check.
        std::fs::write(&done.path, b"changed").unwrap();
        assert!(verify_file(&done.file, package.size, &sha).is_err());
        remove_folder(&done.dir);
        assert!(!done.dir.exists());

        let wrong = hex(&Sha256::digest(b"other"));
        assert!(
            save_package(
                Cursor::new(data.clone()),
                &root,
                &package,
                &wrong,
                &mut |_| {}
            )
            .is_err()
        );
        let long = vec![7u8; 200_001];
        assert!(save_package(Cursor::new(long), &root, &package, &sha, &mut |_| {}).is_err());
        let short = vec![7u8; 199_999];
        assert!(save_package(Cursor::new(short), &root, &package, &sha, &mut |_| {}).is_err());
        let left: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        assert!(left.is_empty(), "failed downloads leave no folder");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn old_download_folders_are_discarded() {
        let root = temp("discard");
        let old = root.join("update-999999999-1-0");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("x"), b"x").unwrap();
        let other = root.join("keep-me");
        std::fs::create_dir_all(&other).unwrap();
        discard_old_downloads(&root, None);
        assert!(!old.exists());
        assert!(other.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tarball_installs_only_the_three_files() {
        let work = temp("tar-work");
        let home = temp("tar-home");
        let src = work.join("src");
        let top = src.join("wavelane-0.2.0-x86_64-linux/usr");
        for dir in [
            "bin",
            "share/applications",
            "share/icons/hicolor/scalable/apps",
            "lib/udev",
        ] {
            std::fs::create_dir_all(top.join(dir)).unwrap();
        }
        std::fs::write(top.join("bin/wavelane"), b"#!/bin/sh\n").unwrap();
        std::fs::write(
            top.join("share/applications/digital.zombie.wavelane.desktop"),
            b"[Desktop Entry]\n",
        )
        .unwrap();
        std::fs::write(
            top.join("share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg"),
            b"<svg/>",
        )
        .unwrap();
        std::fs::write(top.join("lib/udev/rule"), b"rule").unwrap();
        let archive = work.join("a.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&src)
            .arg("wavelane-0.2.0-x86_64-linux")
            .status()
            .unwrap();
        assert!(status.success());
        let cache = work.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        install_tarball(&archive, &cache, &home, "0.2.0").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| {
            std::fs::metadata(home.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(".local/bin/wavelane"), 0o755);
        assert_eq!(
            mode(".local/share/applications/digital.zombie.wavelane.desktop"),
            0o644
        );
        assert_eq!(
            mode(".local/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg"),
            0o644
        );
        assert!(!home.join(".local/lib").exists());
        assert!(
            std::fs::read_dir(&cache).unwrap().next().is_none(),
            "scratch removed"
        );
        assert!(install_tarball(&archive, &cache, &home, "0.3.0").is_err());
        let _ = std::fs::remove_dir_all(work);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn symlinks_are_rejected() {
        let dir = temp("symlink");
        std::fs::write(dir.join("real"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        assert!(regular_file(&dir, &["real"], 10).is_ok());
        assert!(regular_file(&dir, &["link"], 10).is_err());
        assert!(regular_file(&dir, &["real"], 0).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Live read-only GET of the real release list; run explicitly.
    #[test]
    #[ignore]
    fn live_release_list_parses() {
        let outcome = fetch_release(None, InstallKind::Rpm);
        let cache = outcome.cache.expect("etag cached");
        let found = outcome.result.unwrap();
        eprintln!("candidate vs {CURRENT_VERSION}: {found:?}");
        let all = parse_releases(&cache.body, InstallKind::Rpm, "0.0.1")
            .unwrap()
            .unwrap();
        eprintln!(
            "highest: {} notes={} package={:?}",
            all.version,
            all.notes.len(),
            all.package
        );
        assert!(all.package.is_some());
        let tar = parse_releases(&cache.body, InstallKind::Tarball, "0.0.1")
            .unwrap()
            .unwrap();
        assert!(tar.package.is_some());
        let again = fetch_release(Some(cache), InstallKind::Rpm);
        assert!(again.result.is_ok());
    }

    #[test]
    fn pacman_and_preview_names() {
        assert!(package_name_ok(InstallKind::Pacman, "0.2.0", "wavelane-0.2.0-1-x86_64.pkg.tar.zst"));
        assert!(!package_name_ok(InstallKind::Pacman, "0.2.0", "wavelane-0.2.0-1-x86_64.pkg.tar.zst.sig"));
        assert!(!package_name_ok(InstallKind::Pacman, "0.2.0", "wavelane-0.2.0-x86_64.pkg.tar.zst"));
        assert!(package_name_ok(InstallKind::Pacman, "0.2.0-preview.3", "wavelane-0.2.0preview.3-1-x86_64.pkg.tar.zst"));
        assert!(package_name_ok(InstallKind::Rpm, "0.2.0-preview.3", "wavelane-0.2.0~preview.3-1.fc44.x86_64.rpm"));
        assert!(package_name_ok(InstallKind::Deb, "0.2.0-preview.3", "wavelane_0.2.0~preview.3-1_amd64.deb"));
        assert!(package_name_ok(InstallKind::Tarball, "0.2.0-preview.3", "wavelane-0.2.0-preview.3-x86_64-linux.tar.gz"));
    }
}