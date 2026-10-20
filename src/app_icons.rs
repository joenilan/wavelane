//! Application icons for the Sound map, found the way a desktop does: the
//! icon name the application gives, else its launcher (.desktop) entry,
//! looked up in the user's icon theme, the themes it inherits, hicolor and
//! pixmaps (Flatpak exports included). Results are cached for the session.
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// An icon file for an application: its icon name, process binary and
/// display name, in that order of preference.
pub(crate) fn find(icon: &str, binary: &str, name: &str) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let key = format!("{icon}\u{1}{binary}\u{1}{name}");
    let cache = CACHE.get_or_init(Default::default);
    if let Some(found) = cache.lock().ok()?.get(&key) {
        return found.clone();
    }
    let found = resolve(icon, binary, name);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, found.clone());
    }
    found
}

fn resolve(icon: &str, binary: &str, name: &str) -> Option<PathBuf> {
    let binary = Path::new(binary).file_name().and_then(|b| b.to_str()).unwrap_or(binary);
    let binary = binary.strip_suffix(".exe").unwrap_or(binary);
    // An icon name the application gives wins; then its launcher entry.
    if let Some(path) = (!icon.is_empty()).then(|| lookup(icon)).flatten() {
        return Some(path);
    }
    let launchers = launchers();
    for candidate in [binary, name] {
        let candidate = candidate.trim().to_lowercase();
        if candidate.is_empty() {
            continue;
        }
        if let Some(path) = launchers.get(&candidate).and_then(|icon| lookup(icon)) {
            return Some(path);
        }
    }
    // Some applications are installed under their own name as the icon.
    [binary, name]
        .iter()
        .map(|c| c.trim().to_lowercase().replace(' ', "-"))
        .filter(|c| !c.is_empty())
        .find_map(|c| lookup(&c))
}

/// Base directories that hold `icons/` and `applications/`.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from));
        dirs.push(home.join(".local/share/flatpak/exports/share"));
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs.dedup();
    dirs
}

/// Launcher entries by lower-case file name, StartupWMClass, Exec binary
/// and Name, each to its Icon value.
fn launchers() -> &'static HashMap<String, String> {
    static LAUNCHERS: OnceLock<HashMap<String, String>> = OnceLock::new();
    LAUNCHERS.get_or_init(|| {
        let mut map = HashMap::new();
        // Later directories have lower priority: insert only when absent.
        for dir in data_dirs() {
            let Ok(entries) = fs::read_dir(dir.join("applications")) else { continue };
            for entry in entries.flatten().take(4000) {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else { continue };
                let (mut icon, mut class, mut exec, mut name) = (None, None, None, None);
                let mut main = false;
                for line in text.lines() {
                    if line.starts_with('[') {
                        main = line == "[Desktop Entry]";
                        continue;
                    }
                    if !main {
                        continue;
                    }
                    let Some((k, v)) = line.split_once('=') else { continue };
                    match k.trim() {
                        "Icon" => icon = Some(v.trim().to_owned()),
                        "StartupWMClass" => class = Some(v.trim().to_lowercase()),
                        "Exec" => {
                            exec = v
                                .split_whitespace()
                                .find(|w| !w.contains('=') && *w != "env" && !w.starts_with('-'))
                                .and_then(|w| Path::new(w.trim_matches('"')).file_name()?.to_str().map(str::to_lowercase))
                        }
                        "Name" => name = Some(v.trim().to_lowercase()),
                        _ => {}
                    }
                }
                let Some(icon) = icon.filter(|i| !i.is_empty()) else { continue };
                let stem = path.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase);
                // A reverse-DNS id (com.spotify.Client) also by its last part.
                let last = stem.as_ref().and_then(|s| s.rsplit('.').next().map(str::to_owned));
                for key in [stem, last, class, exec, name].into_iter().flatten() {
                    map.entry(key).or_insert_with(|| icon.clone());
                }
            }
        }
        map
    })
}

/// The icon theme in use, then the themes it inherits, then hicolor.
fn themes() -> &'static Vec<PathBuf> {
    static THEMES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let bases: Vec<PathBuf> = data_dirs().into_iter().map(|d| d.join("icons")).collect();
        let home_icons = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".icons"));
        let bases: Vec<PathBuf> = home_icons.into_iter().chain(bases).collect();
        let mut queue = vec![current_theme()];
        let mut seen: Vec<String> = Vec::new();
        let mut dirs = Vec::new();
        while let Some(theme) = queue.pop() {
            if theme.is_empty() || seen.contains(&theme) || seen.len() > 12 {
                continue;
            }
            seen.push(theme.clone());
            let mut inherits = Vec::new();
            for base in &bases {
                let dir = base.join(&theme);
                if dir.is_dir() {
                    if inherits.is_empty() {
                        if let Ok(index) = fs::read_to_string(dir.join("index.theme")) {
                            inherits = index
                                .lines()
                                .find_map(|l| l.strip_prefix("Inherits="))
                                .map(|v| v.split(',').map(|t| t.trim().to_owned()).collect())
                                .unwrap_or_default();
                        }
                    }
                    dirs.push(dir);
                }
            }
            for parent in inherits.into_iter().rev() {
                queue.push(parent);
            }
            if queue.is_empty() && !seen.iter().any(|t| t == "hicolor") {
                queue.push("hicolor".into());
            }
        }
        dirs
    })
}

/// KDE's icon theme, else GTK's, else hicolor.
fn current_theme() -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
    let read = |file: &str, section: &str, key: &str| -> Option<String> {
        let text = fs::read_to_string(config.join(file)).ok()?;
        let mut inside = false;
        for line in text.lines() {
            if line.starts_with('[') {
                inside = line == section;
            } else if inside {
                if let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                    return Some(v.trim().to_owned());
                }
            }
        }
        None
    };
    read("kdeglobals", "[Icons]", "Theme")
        .or_else(|| read("gtk-3.0/settings.ini", "[Settings]", "gtk-icon-theme-name"))
        .unwrap_or_else(|| "hicolor".into())
}

/// An icon by name (or an absolute path) in the themes, largest useful
/// size first, then pixmaps.
fn lookup(icon: &str) -> Option<PathBuf> {
    if icon.starts_with('/') {
        return Path::new(icon).is_file().then(|| PathBuf::from(icon));
    }
    if icon.contains('/') || icon.contains("..") {
        return None;
    }
    const SUBDIRS: [&str; 14] = [
        "scalable/apps", "128x128/apps", "96x96/apps", "64x64/apps", "48x48/apps", "256x256/apps",
        "512x512/apps", "32x32/apps", "apps/scalable", "apps/64", "apps/48", "apps/96", "apps/128", "apps/32",
    ];
    for theme in themes() {
        for sub in SUBDIRS {
            for ext in ["svg", "png"] {
                let path = theme.join(sub).join(format!("{icon}.{ext}"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    data_dirs().into_iter().chain([PathBuf::from("/usr/share")]).find_map(|d| {
        ["png", "svg", "xpm"]
            .iter()
            .map(|ext| d.join("pixmaps").join(format!("{icon}.{ext}")))
            .find(|p| p.is_file() && !p.to_string_lossy().ends_with(".xpm"))
    })
}

#[cfg(test)]
mod tests {
    /// Prints what this machine resolves; run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn resolves_installed_applications() {
        for (icon, binary, name) in [
            ("", "spotify", "Spotify"),
            ("", "chrome", "Google Chrome"),
            ("", "Discord", "Discord"),
            ("", "RocketLeague.exe", "Rocket League"),
            ("firefox", "firefox", "Firefox"),
            ("", "jawjack", "Jawjack"),
            ("", "pw-play", "MapS1"),
        ] {
            println!("{name:>14}: {:?}", super::find(icon, binary, name));
        }
    }
}
