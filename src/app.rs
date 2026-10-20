//! Wavelane lives in the tray; the map opens over the screen from the tray,
//! the system-wide shortcut, or another launch (`wavelane`, `wavelane --map`).
use crate::engine::Worker;
use gpui_kit::*;
use std::{
    borrow::Cow,
    collections::BTreeSet,
    fs,
    path::PathBuf,
    time::Duration,
};

/// Control-safe identifier for an application key or output name.
pub(crate) fn key_id(key: &str) -> String {
    key.bytes().map(|b| format!("{b:02x}")).collect()
}

/// App-wide state: the engine, and the map window while it is open.
pub(crate) struct Core {
    pub(crate) worker: Worker,
    /// Outputs hidden from the map (shown again while editing devices).
    pub(crate) hidden_outputs: BTreeSet<String>,
    pub(crate) map_window: Option<AnyWindowHandle>,
    /// The tray's volume flyout, while open.
    pub(crate) flyout_window: Option<AnyWindowHandle>,
    pub(crate) map_minimal: bool,
    /// The map draws light (no blur, shadows, glows or flowing cables): the
    /// CPU renderer, or slow frames measured when it last opened.
    pub(crate) map_light: bool,
    /// The open map should fade out and close.
    pub(crate) map_close_requested: bool,
    pub(crate) hotkey: crate::hotkey::Hotkey,
    pub(crate) updates: crate::updates::Updates,
    /// Asked for from the tray: the flyout shows the check's result.
    pub(crate) update_asked: bool,
    /// Update clicked: install as soon as the download is verified.
    pub(crate) install_when_ready: bool,
    tray: crate::tray::Tray,
    instance: Option<crate::single_instance::Instance>,
}
impl Global for Core {}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("wavelane"))
}
/// Hidden outputs: `~/.config/wavelane/hidden-outputs`, one hex name a line.
fn load_hidden() -> BTreeSet<String> {
    let Some(text) = config_dir().and_then(|d| fs::read_to_string(d.join("hidden-outputs")).ok()) else {
        return BTreeSet::new();
    };
    let mut lines = text.lines();
    if lines.next() != Some("wavelane-hidden-outputs=1") {
        return BTreeSet::new();
    }
    lines
        .filter_map(|hex| {
            if hex.is_empty() || hex.len() % 2 != 0 {
                return None;
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                .collect::<Option<Vec<_>>>()?;
            String::from_utf8(bytes).ok()
        })
        .collect()
}
pub(crate) fn save_hidden(hidden: &BTreeSet<String>) {
    let Some(dir) = config_dir() else { return };
    let mut text = String::from("wavelane-hidden-outputs=1\n");
    for name in hidden {
        text.push_str(&key_id(name));
        text.push('\n');
    }
    let temp = dir.join(format!(".hidden-outputs-{}", std::process::id()));
    if fs::create_dir_all(&dir).is_ok() && fs::write(&temp, text).is_ok() {
        let _ = fs::rename(&temp, dir.join("hidden-outputs"));
    }
}

/// The map's small line icons, drawn in the current text color.
struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let shape = match path {
            "console/speaker.svg" => "M11 5 6 9H3v6h3l5 4ZM15 8a6 6 0 0 1 0 8m3-11a10 10 0 0 1 0 14",
            "console/headphones.svg" => "M3 18v-6a9 9 0 0 1 18 0v6M21 19a2 2 0 0 1-2 2h-1v-6h3ZM3 19a2 2 0 0 0 2 2h1v-6H3Z",
            "console/monitor.svg" => "M4 4h16a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM8 21h8M12 17v4",
            "console/orbit.svg" => "M12 9a3 3 0 1 1 0 6 3 3 0 0 1 0-6ZM4.5 7.5a9 9 0 0 1 13-2.6M19.5 16.5a9 9 0 0 1-13 2.6M19 5a1.5 1.5 0 1 1 0 .1M5 19a1.5 1.5 0 1 1 0 .1",
            "console/close.svg" => "M6 6l12 12M18 6 6 18",
            "console/muted.svg" => "M11 5 6 9H3v6h3l5 4ZM16 9l5 6M21 9l-5 6",
            _ => return Ok(None),
        };
        Ok(Some(Cow::Owned(
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><path d=\"{shape}\" fill=\"none\" stroke=\"black\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>"
            )
            .into_bytes(),
        )))
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// Run Wavelane: the engine, tray, shortcut and (unless started hidden) the
/// map right away.
pub fn run(hidden: bool, instance: Option<crate::single_instance::Instance>) {
    application().with_assets(Assets).run(move |cx| {
        init(cx);
        cx.set_quit_mode(QuitMode::Explicit);
        cx.set_global(Core {
            worker: Worker::spawn(),
            hidden_outputs: load_hidden(),
            map_window: None,
            flyout_window: None,
            map_minimal: false,
            map_light: std::env::var("VK_DRIVER_FILES").is_ok_and(|f| f.contains("lvp_icd")),
            map_close_requested: false,
            hotkey: crate::hotkey::Hotkey::spawn(),
            updates: crate::updates::Updates::load(),
            update_asked: false,
            install_when_ready: false,
            tray: crate::tray::Tray::spawn(),
            instance,
        });
        cx.on_window_closed(|cx, _| {
            let open = cx.windows();
            let core = cx.global_mut::<Core>();
            if core.map_window.is_some_and(|w| !open.contains(&w)) {
                core.map_window = None;
            }
            if core.flyout_window.is_some_and(|w| !open.contains(&w)) {
                core.flyout_window = None;
            }
        })
        .detach();
        if !hidden {
            crate::map::toggle(None, cx);
        }
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let quit = cx.update(tick);
            if quit {
                let _ = cx.update(|cx| {
                    cx.global_mut::<Core>().worker.stop();
                    cx.quit();
                });
                break;
            }
        })
        .detach();
    });
}

/// Update checks: the feed, the tray menu, and the flyout while it shows them.
fn update_tick(actions: u8, cx: &mut App) {
    use crate::updates::{Phase, UpdateMode};
    let core = cx.global_mut::<Core>();
    if actions & crate::tray::CHECK != 0 {
        core.update_asked = true;
        core.updates.check_now();
    }
    if actions & crate::tray::AUTO_CHECK != 0 {
        let mode = match core.updates.view().mode {
            UpdateMode::Notify => UpdateMode::Off,
            UpdateMode::Off => UpdateMode::Notify,
        };
        if let Err(error) = core.updates.set_mode(mode) {
            eprintln!("Wavelane updates: {error}");
        }
    }
    let mut changed = core.updates.poll();
    let _ = core.updates.take_announcement();
    let view = core.updates.view();
    if core.install_when_ready && view.phase == Phase::Ready {
        core.install_when_ready = false;
        core.updates.start_install();
        changed = true;
    }
    let latest = view.latest.clone().filter(|_| !matches!(view.phase, Phase::UpToDate | Phase::Checking | Phase::Idle));
    core.tray.set_update(crate::tray::UpdateMenu { latest, automatic: view.mode == UpdateMode::Notify });
    let open = core.flyout_window;
    if actions & crate::tray::CHECK != 0 && open.is_none_or(|w| !cx.windows().contains(&w)) {
        crate::flyout::toggle(cx);
    } else if changed {
        if let Some(handle) = open {
            let _ = handle.update(cx, |_, window, _| window.refresh());
        }
    }
}

/// Requests from the tray, the shortcut and later launches. True quits.
fn tick(cx: &mut App) -> bool {
    let requests = cx
        .global::<Core>()
        .instance
        .as_ref()
        .map(|i| i.requests())
        .unwrap_or_default();
    for request in requests {
        match request {
            crate::single_instance::Request::Map(token) => crate::map::toggle(token, cx),
            // Launching it again (the application menu): the flyout.
            crate::single_instance::Request::Show(_) => crate::flyout::toggle(cx),
            crate::single_instance::Request::Hidden => {}
        }
    }
    if cx.global::<Core>().hotkey.take() {
        crate::map::toggle(None, cx);
    }
    let actions = cx.global::<Core>().tray.actions();
    if actions & crate::tray::QUIT != 0 {
        return true;
    }
    if actions & crate::tray::FLYOUT != 0 {
        crate::flyout::toggle(cx);
    }
    if actions & crate::tray::MAP != 0 {
        let token = cx.global::<Core>().tray.take_activation_token();
        crate::map::toggle(token, cx);
    }
    update_tick(actions, cx);
    // Without a tray host there is no way back after closing the map.
    let _ = cx.global::<Core>().tray.available();
    false
}
