//! System-wide shortcut for the map, through the desktop's Global Shortcuts
//! portal: KDE asks once to confirm it, and the key can be changed later in
//! System Settings → Shortcuts. Without the portal, `wavelane --map` can be
//! bound to a key by hand.
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

const SHORTCUT: &str = "toggle-map";
/// Suggested key; the desktop may assign another or none. Development
/// builds (`--features dev-shortcut`) suggest another, so they can run next
/// to LinX, which suggests Ctrl+Alt+M too.
#[cfg(not(feature = "dev-shortcut"))]
const PREFERRED: &str = "CTRL+ALT+M";
#[cfg(feature = "dev-shortcut")]
const PREFERRED: &str = "CTRL+ALT+W";

pub(crate) struct Hotkey {
    pressed: Arc<AtomicBool>,
    /// The key the desktop bound, as it describes it ("Ctrl+Alt+M").
    trigger: Arc<Mutex<Option<String>>>,
}
impl Hotkey {
    pub(crate) fn spawn() -> Self {
        let pressed = Arc::new(AtomicBool::new(false));
        let trigger = Arc::new(Mutex::new(None));
        let (flag, bound) = (pressed.clone(), trigger.clone());
        let _ = std::thread::Builder::new().name("wavelane-hotkey".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => return eprintln!("Wavelane shortcut unavailable: {error}"),
            };
            if let Err(error) = runtime.block_on(listen(flag, bound)) {
                eprintln!("Wavelane shortcut unavailable: {error}");
            }
        });
        Self { pressed, trigger }
    }
    /// The bound key, once the desktop has answered and when it gave one.
    pub(crate) fn trigger(&self) -> Option<String> {
        self.trigger.lock().ok()?.clone()
    }
    /// The shortcut was pressed since the last call.
    pub(crate) fn take(&self) -> bool {
        self.pressed.swap(false, Ordering::AcqRel)
    }
}

async fn listen(pressed: Arc<AtomicBool>, trigger: Arc<Mutex<Option<String>>>) -> ashpd::Result<()> {
    // A connection of its own: the portal ties the app ID to the connection
    // that registers it, before any other portal call on it.
    let connection = ashpd::zbus::Connection::session().await?;
    if let Ok(app) = ashpd::AppID::try_from(crate::app::app_id()) {
        if let Err(error) = ashpd::register_host_app_with_connection(connection.clone(), app).await {
            eprintln!("Wavelane shortcut: app registration: {error}");
        }
    }
    let portal = GlobalShortcuts::with_connection(connection).await?;
    let session = portal.create_session(Default::default()).await?;
    let mut activated = portal.receive_activated().await?;
    let shortcut = NewShortcut::new(SHORTCUT, crate::app::title()).preferred_trigger(PREFERRED);
    let bound = portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await?
        .response()?;
    for s in bound.shortcuts() {
        eprintln!("Wavelane shortcut: {} = {}", s.id(), s.trigger_description());
        let description = s.trigger_description().trim();
        if s.id() == SHORTCUT && !description.is_empty() && !description.eq_ignore_ascii_case("none") {
            if let Ok(mut t) = trigger.lock() {
                *t = Some(description.to_owned());
            }
        }
    }
    while let Some(event) = activated.next().await {
        if event.shortcut_id() == SHORTCUT {
            pressed.store(true, Ordering::Release);
        }
    }
    Ok(())
}
