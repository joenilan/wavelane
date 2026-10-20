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

struct Item {
    actions: Arc<AtomicU8>,
    /// xdg-activation token from the tray host for the latest click.
    token: Arc<Mutex<Option<String>>>,
}
impl ksni::Tray for Item {
    fn provide_xdg_activation_token(&mut self, token: String) {
        *self.token.lock().unwrap() = Some(token);
    }
    fn id(&self) -> String {
        "wavelane".into()
    }
    fn title(&self) -> String {
        "Wavelane".into()
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
            title: "Wavelane".into(),
            description: "Click to see where every app's sound goes".into(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.actions.fetch_or(FLYOUT, Ordering::Release);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open map".into(),
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(MAP, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|item: &mut Self| {
                    item.actions.fetch_or(QUIT, Ordering::Release);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct Tray {
    actions: Arc<AtomicU8>,
    token: Arc<Mutex<Option<String>>>,
    available: Arc<Mutex<Option<bool>>>,
}
impl Tray {
    pub fn spawn() -> Self {
        let actions = Arc::new(AtomicU8::new(0));
        let token = Arc::new(Mutex::new(None));
        let available = Arc::new(Mutex::new(None));
        let (flags, click, ready) = (actions.clone(), token.clone(), available.clone());
        std::thread::spawn(move || match (Item { actions: flags, token: click }).spawn() {
            Ok(handle) => {
                *ready.lock().unwrap() = Some(true);
                while !handle.is_closed() {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                *ready.lock().unwrap() = Some(false);
            }
            Err(_) => *ready.lock().unwrap() = Some(false),
        });
        Self { actions, token, available }
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
