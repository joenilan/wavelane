//! Tray icon (StatusNotifierItem): a click opens the map. Bus work never runs
//! on the render thread.
use ksni::blocking::TrayMethods;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Mutex,
};

/// "Open map" from the menu.
pub const MAP: u8 = 1;
pub const QUIT: u8 = 2;
/// A click on the icon: the volume flyout.
pub const FLYOUT: u8 = 4;
/// "Check for updates".
pub const CHECK: u8 = 8;
/// "Check for updates automatically" toggled.
pub const AUTO_CHECK: u8 = 16;

/// What the menu shows about updates: the newer version, and whether
/// checks are automatic.
#[derive(Clone, Default, PartialEq)]
pub struct UpdateMenu {
    pub latest: Option<String>,
    pub automatic: bool,
}

struct Item {
    actions: Arc<AtomicU8>,
    /// xdg-activation token from the tray host for the latest click.
    token: Arc<Mutex<Option<String>>>,
    update: Arc<Mutex<UpdateMenu>>,
}
impl ksni::Tray for Item {
    fn provide_xdg_activation_token(&mut self, token: String) {
        *self.token.lock().unwrap() = Some(token);
    }
    fn id(&self) -> String {
        if cfg!(feature = "dev-shortcut") { "wavelane-dev" } else { "wavelane" }.into()
    }
    fn title(&self) -> String {
        if cfg!(feature = "dev-shortcut") {
            crate::app::title()
        } else if crate::updates::dev_build() {
            "Wavelane DEV"
        } else {
            "Wavelane"
        }.into()
    }
    fn icon_name(&self) -> String {
        // The drawn symbolic icon, which suits a panel, rather than the
        // colourful application icon.
        String::new()
    }
    /// Drawn here too: a tray host may not find a freshly installed icon.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon_pixmaps()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.title(),
            description: "Click to see where every app's sound goes".into(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.actions.fetch_or(FLYOUT, Ordering::Release);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};
        let update = self.update.lock().map(|u| u.clone()).unwrap_or_default();
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();
        if let Some(latest) = &update.latest {
            items.push(
                StandardItem {
                    label: format!("Update to {latest}…"),
                    activate: Box::new(|item: &mut Self| {
                        item.actions.fetch_or(FLYOUT, Ordering::Release);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items.push(ksni::MenuItem::Separator);
        }
        items.extend([
            StandardItem {
                label: "Open map".into(),
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(MAP, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Launch at login".into(),
                checked: autostart_path().is_some_and(|p| p.exists()),
                activate: Box::new(|_: &mut Self| set_autostart(!autostart_path().is_some_and(|p| p.exists()))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Check for updates".into(),
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(CHECK, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Check for updates automatically".into(),
                checked: update.automatic,
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(AUTO_CHECK, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(QUIT, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

pub struct Tray {
    actions: Arc<AtomicU8>,
    token: Arc<Mutex<Option<String>>>,
    available: Arc<Mutex<Option<bool>>>,
    update: Arc<Mutex<UpdateMenu>>,
    /// The menu changed; the tray thread tells the host.
    dirty: Arc<std::sync::atomic::AtomicBool>,
}
impl Tray {
    pub fn spawn() -> Self {
        let actions = Arc::new(AtomicU8::new(0));
        let token = Arc::new(Mutex::new(None));
        let available = Arc::new(Mutex::new(None));
        let update = Arc::new(Mutex::new(UpdateMenu::default()));
        let dirty = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (flags, click, ready) = (actions.clone(), token.clone(), available.clone());
        let (menu, changed) = (update.clone(), dirty.clone());
        std::thread::spawn(move || match (Item { actions: flags, token: click, update: menu }).spawn() {
            Ok(handle) => {
                *ready.lock().unwrap() = Some(true);
                while !handle.is_closed() {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if changed.swap(false, Ordering::AcqRel) {
                        handle.update(|_| {});
                    }
                }
                *ready.lock().unwrap() = Some(false);
            }
            Err(_) => *ready.lock().unwrap() = Some(false),
        });
        Self { actions, token, available, update, dirty }
    }
    /// What the menu says about updates.
    pub fn set_update(&self, menu: UpdateMenu) {
        if let Ok(mut current) = self.update.lock() {
            if *current != menu {
                *current = menu;
                self.dirty.store(true, Ordering::Release);
            }
        }
    }
    pub fn actions(&self) -> u8 {
        self.actions.swap(0, Ordering::AcqRel)
    }
    pub fn take_activation_token(&self) -> Option<String> {
        self.token.lock().unwrap().take()
    }
    /// None until the tray host answered.
    pub fn available(&self) -> Option<bool> {
        *self.available.lock().unwrap()
    }
}

/// The app icon at tray sizes, as ARGB32 (network byte order).
fn icon_pixmaps() -> Vec<ksni::Icon> {
    use resvg::{tiny_skia, usvg};
    let Ok(tree) = usvg::Tree::from_str(
        include_str!("../packaging/wavelane-tray-symbolic.svg"),
        &usvg::Options::default(),
    ) else {
        return Vec::new();
    };
    [22u32, 32, 48, 64]
        .into_iter()
        .filter_map(|size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale = size as f32 / tree.size().width();
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            let data = pixmap
                .pixels()
                .iter()
                .flat_map(|p| {
                    let c = p.demultiply();
                    [c.alpha(), c.red(), c.green(), c.blue()]
                })
                .collect();
            Some(ksni::Icon { width: size as i32, height: size as i32, data })
        })
        .collect()
}

/// `~/.config/autostart/digital.zombie.wavelane.desktop`.
fn autostart_path() -> Option<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join("digital.zombie.wavelane.desktop"))
}

/// Start Wavelane in the tray at login (this executable), or stop doing so.
fn set_autostart(on: bool) {
    let Some(path) = autostart_path() else { return };
    if !on {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Wavelane\nComment=Where every app's sound goes\nExec=\"{}\" --hidden\nIcon=digital.zombie.wavelane\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
        exe.display()
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, entry);
}
