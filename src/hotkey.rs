//! System-wide shortcut for the map, through the desktop's Global Shortcuts
//! portal: KDE asks once to confirm it, and the key can be changed later in
//! System Settings → Shortcuts. Without the portal, `wavelane --map` can be
//! bound to a key by hand.
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const SHORTCUT: &str = "toggle-map";
/// Suggested key; the desktop may assign another or none.
const PREFERRED: &str = "CTRL+ALT+W";

pub(crate) struct Hotkey {
    pressed: Arc<AtomicBool>,
}
impl Hotkey {
    pub(crate) fn spawn() -> Self {
        let pressed = Arc::new(AtomicBool::new(false));
        let flag = pressed.clone();
        let _ = std::thread::Builder::new().name("wavelane-hotkey".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => return eprintln!("Wavelane shortcut unavailable: {error}"),
            };
            if let Err(error) = runtime.block_on(listen(flag)) {
                eprintln!("Wavelane shortcut unavailable: {error}");
            }
        });
        Self { pressed }
    }
    /// The shortcut was pressed since the last call.
    pub(crate) fn take(&self) -> bool {
        self.pressed.swap(false, Ordering::AcqRel)
    }
}

async fn listen(pressed: Arc<AtomicBool>) -> ashpd::Result<()> {
    // A connection of its own: the portal ties the app ID to the connection
    // that registers it, before any other portal call on it.
    let connection = ashpd::zbus::Connection::session().await?;
    if let Ok(app) = ashpd::AppID::try_from("digital.zombie.wavelane") {
        if let Err(error) = ashpd::register_host_app_with_connection(connection.clone(), app).await {
            eprintln!("Wavelane shortcut: app registration: {error}");
        }
    }
    let portal = GlobalShortcuts::with_connection(connection).await?;
    let session = portal.create_session(Default::default()).await?;
    let mut activated = portal.receive_activated().await?;
    let shortcut = NewShortcut::new(SHORTCUT, "Open or close Wavelane").preferred_trigger(PREFERRED);
    let bound = portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await?
        .response()?;
    for s in bound.shortcuts() {
        eprintln!("Wavelane shortcut: {} = {}", s.id(), s.trigger_description());
    }
    while let Some(event) = activated.next().await {
        if event.shortcut_id() == SHORTCUT {
            pressed.store(true, Ordering::Release);
        }
    }
    Ok(())
}
