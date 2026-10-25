//! Wavelane's engine: one worker thread owns the audio graph, applies each
//! application's placement to its streams (new ones included), and publishes
//! a plain view for the UI. Placements persist per application in
//! `~/.config/wavelane/placements`.
use crate::graph::{AudioGraph, Output, Snapshot, Stream};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Starting,
    Running,
    /// PipeWire unavailable; retrying.
    Retrying,
}

/// One application: every output stream with the same identity.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct App {
    pub key: String,
    pub name: String,
    pub icon: String,
    pub binary: String,
    pub running: bool,
    pub streams: usize,
    /// The output every stream plays on now, when they agree.
    pub playing_on: Option<String>,
    /// The output the user placed it on; `None` follows the default.
    pub pinned: Option<String>,
    pub volume: Option<f32>,
    pub mute: bool,
    /// Node ids of its streams, for live levels.
    pub nodes: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub phase: Phase,
    pub error: Option<String>,
    /// The last command's failure, if any.
    pub message: Option<String>,
    pub apps: Vec<App>,
    pub outputs: Vec<Output>,
    /// The output in use as the default (node.name).
    pub default_output: Option<String>,
}

impl View {
    /// Whether anything a window shows differs, live levels aside.
    pub fn differs(&self, other: &View) -> bool {
        fn shown(o: &crate::graph::Output) -> (u32, u64, &str, &str, &str, &str, &str, u64, &str, Option<f32>, bool, &str) {
            (o.node, o.serial, &o.name, &o.description, &o.nick, &o.icon,
                &o.form_factor, o.device, &o.device_description, o.volume, o.mute, &o.group)
        }
        self.phase != other.phase
            || self.error != other.error
            || self.message != other.message
            || self.apps != other.apps
            || self.default_output != other.default_output
            || !self.outputs.iter().map(shown).eq(other.outputs.iter().map(shown))
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    /// Place one application (key) on an output, or `None` to follow the
    /// default output.
    Place(String, Option<String>),
    /// Everything: an output becomes the default and every application
    /// follows it (`None`: only the placements end).
    PlaceAll(Option<String>),
    /// An application's own volume (linear, up to 1.5) and/or mute.
    AppLevel { key: String, volume: Option<f32>, mute: Option<bool> },
    /// An output's volume and/or mute.
    OutputLevel { name: String, volume: Option<f32>, mute: Option<bool> },
    /// Live levels on or off.
    Metering(bool),
}

/// Placements and released applications: `~/.config/wavelane/placements`.
pub struct Store {
    path: Option<PathBuf>,
}
impl Store {
    pub fn user() -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        Self { path: base.map(|b| b.join("wavelane").join("placements")) }
    }
    pub fn load(&self) -> (BTreeMap<String, String>, BTreeSet<String>) {
        let Some(path) = &self.path else { return Default::default() };
        let Ok(text) = fs::read_to_string(path) else { return Default::default() };
        if !text.starts_with("wavelane-placements=1\n") {
            // Unreadable (newer or damaged): kept aside, not overwritten.
            let aside = path.with_extension("unreadable");
            eprintln!("Wavelane: placements file not understood; kept as {}", aside.display());
            let _ = fs::rename(path, aside);
            return Default::default();
        }
        parse_store(&text)
    }
    pub fn save(&self, pinned: &BTreeMap<String, String>, released: &BTreeSet<String>) -> Result<(), String> {
        let path = self.path.as_ref().ok_or("No settings directory.")?;
        let parent = path.parent().ok_or("No settings directory.")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temp = parent.join(format!(".placements-{}", std::process::id()));
        let written = fs::File::create(&temp).and_then(|mut file| {
            file.write_all(format_store(pinned, released).as_bytes())?;
            file.sync_all()
        });
        let result = written.and_then(|_| fs::rename(&temp, path));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|e| e.to_string())
    }
}
fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Option<String> {
    if s.is_empty() || s.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
/// `wavelane-placements=1`, then `device\t<hex output>\t<hex app>` and
/// `released\t-\t<hex app>` lines.
pub fn format_store(pinned: &BTreeMap<String, String>, released: &BTreeSet<String>) -> String {
    let mut out = String::from("wavelane-placements=1\n");
    for (key, output) in pinned {
        out.push_str(&format!("device\t{}\t{}\n", hex(output), hex(key)));
    }
    for key in released {
        out.push_str(&format!("released\t-\t{}\n", hex(key)));
    }
    out
}
pub fn parse_store(text: &str) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let (mut pinned, mut released) = (BTreeMap::new(), BTreeSet::new());
    let mut lines = text.lines();
    if lines.next() != Some("wavelane-placements=1") {
        return (pinned, released);
    }
    for line in lines {
        let mut f = line.split('\t');
        let (Some(kind), Some(value), Some(key)) = (f.next(), f.next(), f.next()) else { continue };
        let Some(key) = unhex(key) else { continue };
        match kind {
            "device" => {
                if let Some(output) = unhex(value) {
                    pinned.insert(key, output);
                }
            }
            "released" => {
                released.insert(key);
            }
            _ => {}
        }
    }
    (pinned, released)
}

/// Group streams into applications.
pub fn applications(streams: &[Stream], pinned: &BTreeMap<String, String>) -> Vec<App> {
    let mut apps: BTreeMap<String, App> = BTreeMap::new();
    let mut on: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in streams {
        let key = s.app_key();
        let app = apps.entry(key.clone()).or_insert_with(|| App {
            name: key.trim().to_owned(),
            pinned: pinned.get(&key).cloned(),
            key: key.clone(),
            mute: true,
            ..Default::default()
        });
        app.running |= s.running;
        app.streams += 1;
        app.nodes.push(s.node);
        if app.icon.is_empty() {
            app.icon = s.icon.clone();
        }
        if app.binary.is_empty() {
            app.binary = s.binary.clone();
        }
        app.volume = app.volume.or(s.volume);
        app.mute &= s.mute;
        if !s.playing_on.is_empty() {
            on.entry(key).or_default().insert(s.playing_on.clone());
        }
    }
    for app in apps.values_mut() {
        app.playing_on = on.get(&app.key).filter(|o| o.len() == 1).and_then(|o| o.first().cloned());
    }
    apps.into_values().collect()
}

struct Engine {
    graph: AudioGraph,
    pinned: BTreeMap<String, String>,
    released: BTreeSet<String>,
    /// Streams already given their placement.
    handled: BTreeSet<(u32, u64)>,
    present: BTreeSet<String>,
    snapshot: Snapshot,
}

impl Engine {
    fn streams_of(&self, key: &str) -> Vec<Stream> {
        self.snapshot
            .streams
            .iter()
            .filter(|s| s.app_key() == key && s.movable)
            .cloned()
            .collect()
    }

    fn observe(&mut self) -> Result<(), String> {
        self.snapshot = self.graph.snapshot()?;
        let live: BTreeSet<_> = self.snapshot.streams.iter().map(|s| (s.node, s.serial)).collect();
        self.handled.retain(|id| live.contains(id));
        let present: BTreeSet<String> = self.snapshot.outputs.iter().map(|o| o.name.clone()).collect();
        if present != self.present {
            // A device came back: placements on it are applied again. Only
            // those, so routes another program made stay as they are.
            let returned: BTreeSet<&String> = present.difference(&self.present).collect();
            let pinned = &self.pinned;
            let replace: Vec<_> = self
                .snapshot
                .streams
                .iter()
                .filter(|s| pinned.get(&s.app_key()).is_some_and(|o| returned.contains(o)))
                .map(|s| (s.node, s.serial))
                .collect();
            for id in replace {
                self.handled.remove(&id);
            }
            self.present = present;
        }
        Ok(())
    }

    /// Give new streams their application's placement once; release a
    /// leftover target (the session manager's memory of an earlier
    /// placement) for applications that now follow the default.
    fn settle(&mut self) {
        if !self.snapshot.metadata {
            return;
        }
        let streams: Vec<Stream> = self.snapshot.streams.clone();
        // Applications whose leftover target is released this time (every
        // stream of theirs, not only the first).
        let mut releasing = BTreeSet::new();
        for s in streams.iter().filter(|s| s.info && s.movable) {
            let id = (s.node, s.serial);
            if self.handled.contains(&id) {
                continue;
            }
            let key = s.app_key();
            match self.pinned.get(&key).cloned() {
                Some(output) if self.present.contains(&output) => {
                    if s.targeted_at != output {
                        if let Err(error) = self.graph.route(s, Some(&output)) {
                            // Not handled: tried again on the next pass.
                            eprintln!("Wavelane: {error}");
                            continue;
                        }
                    }
                    self.handled.insert(id);
                }
                // Its device is unplugged: it plays on the default until
                // the device returns (handled again then).
                Some(_) => {}
                None => {
                    if self.released.remove(&key) {
                        releasing.insert(key.clone());
                    }
                    if releasing.contains(&key) && !s.targeted_at.is_empty() {
                        eprintln!("Wavelane: {key} came back on an output it no longer uses; returning it to the default");
                        let _ = self.graph.route(s, None);
                    }
                    self.handled.insert(id);
                }
            }
        }
    }

    fn place(&mut self, key: &str, output: Option<String>) -> Result<(), String> {
        if let Some(name) = &output {
            if !self.present.contains(name) {
                return Err("That output is no longer available.".into());
            }
        }
        let streams = self.streams_of(key);
        match &output {
            Some(name) => {
                self.pinned.insert(key.to_owned(), name.clone());
                self.released.remove(key);
            }
            None => {
                if self.pinned.remove(key).is_some() && streams.is_empty() {
                    // Closed: the session manager keeps its last output.
                    self.released.insert(key.to_owned());
                }
            }
        }
        let mut result = Ok(());
        for s in &streams {
            if let Err(error) = self.graph.route(s, output.as_deref()) {
                result = Err(error);
            }
            self.handled.insert((s.node, s.serial));
        }
        result
    }

    fn place_all(&mut self, output: Option<String>) -> Result<(), String> {
        if let Some(name) = &output {
            self.graph.set_default_output(name)?;
        }
        // Every application, even when one fails; the first error is kept.
        let mut result = Ok(());
        let keys: Vec<String> = self.pinned.keys().cloned().collect();
        for key in keys {
            if let Err(error) = self.place(&key, None) {
                result = result.and(Err(error));
            }
        }
        // Streams pointed elsewhere by anyone follow the new default too.
        let streams: Vec<Stream> = self.snapshot.streams.iter().filter(|s| s.movable && !s.targeted_at.is_empty()).cloned().collect();
        for s in &streams {
            let _ = self.graph.route(s, None);
        }
        result
    }

    fn app_level(&mut self, key: &str, volume: Option<f32>, mute: Option<bool>) -> Result<(), String> {
        let nodes: Vec<u32> = self.snapshot.streams.iter().filter(|s| s.app_key() == key).map(|s| s.node).collect();
        if nodes.is_empty() {
            return Err("That application stopped playing.".into());
        }
        for node in nodes {
            self.graph.set_level(node, volume, mute)?;
        }
        Ok(())
    }

    fn output_level(&mut self, name: &str, volume: Option<f32>, mute: Option<bool>) -> Result<(), String> {
        let node = self.snapshot.outputs.iter().find(|o| o.name == name).map(|o| o.node).ok_or("That output is no longer available.")?;
        self.graph.set_level(node, volume, mute)
    }

    fn view(&self, view: &mut View) {
        view.phase = Phase::Running;
        view.error = Some(self.snapshot.error.clone()).filter(|e| !e.is_empty());
        view.outputs = self.snapshot.outputs.clone();
        view.default_output = self.snapshot.default_output.clone();
        view.apps = applications(&self.snapshot.streams, &self.pinned);
    }
}

const TICK: Duration = Duration::from_millis(250);
/// Live levels while the map shows them.
const LEVEL_TICK: Duration = Duration::from_millis(33);
const RETRY: Duration = Duration::from_secs(2);

pub struct Worker {
    tx: SyncSender<Command>,
    latest: Arc<Mutex<View>>,
    levels: Arc<Mutex<HashMap<u32, f32>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Command>(64);
        let latest = Arc::new(Mutex::new(View::default()));
        let levels = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (view, lv, ending) = (latest.clone(), levels.clone(), stop.clone());
        let thread = thread::Builder::new()
            .name("wavelane-engine".into())
            .spawn(move || run(rx, view, lv, ending))
            .ok();
        Self { tx, latest, levels, stop, thread }
    }
    pub fn view(&self) -> View {
        self.latest.lock().map(|v| v.clone()).unwrap_or_default()
    }
    /// Live level (peak 0..1) of a node: an output or an application stream.
    pub fn level(&self, node: u32) -> Option<f32> {
        self.levels.lock().ok()?.get(&node).copied()
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.tx.try_send(command).map_err(|e| match e {
            TrySendError::Full(_) => "Busy; try again.".into(),
            TrySendError::Disconnected(_) => "Wavelane stopped. Reopen it.".into(),
        })
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(rx: mpsc::Receiver<Command>, latest: Arc<Mutex<View>>, levels: Arc<Mutex<HashMap<u32, f32>>>, ending: Arc<AtomicBool>) {
    let store = Store::user();
    let (mut pinned, mut released) = store.load();
    let mut engine: Option<Engine> = None;
    let mut view = View::default();
    let mut metering = false;
    let mut retry_at = Instant::now();
    let mut next_tick = Instant::now();
    let publish = |view: &View| {
        if let Ok(mut latest) = latest.lock() {
            *latest = view.clone();
        }
    };
    while !ending.load(Ordering::Acquire) {
        let wait = if metering { LEVEL_TICK } else { TICK };
        let deadline = next_tick.min(Instant::now() + wait);
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(command) => {
                let result = match (command, engine.as_mut()) {
                    (Command::Metering(on), e) => {
                        metering = on;
                        if !on {
                            if let Ok(mut levels) = levels.lock() { levels.clear(); }
                        }
                        if let Some(e) = e {
                            e.graph.set_metering(on);
                        }
                        Ok(())
                    }
                    (_, None) => Err("Not connected to PipeWire yet.".to_string()),
                    (Command::Place(key, output), Some(e)) => e.place(&key, output),
                    (Command::PlaceAll(output), Some(e)) => e.place_all(output),
                    (Command::AppLevel { key, volume, mute }, Some(e)) => e.app_level(&key, volume, mute),
                    (Command::OutputLevel { name, volume, mute }, Some(e)) => e.output_level(&name, volume, mute),
                };
                view.message = result.err();
                next_tick = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if metering {
            if let (Some(e), Ok(mut levels)) = (engine.as_mut(), levels.lock()) {
                *levels = e.graph.levels().into_iter().collect();
            }
        }
        if Instant::now() < next_tick {
            continue;
        }
        next_tick = Instant::now() + TICK;
        if engine.is_none() && Instant::now() >= retry_at {
            match AudioGraph::open() {
                Ok(mut graph) => {
                    graph.set_metering(metering);
                    engine = Some(Engine {
                        graph,
                        pinned: pinned.clone(),
                        released: released.clone(),
                        handled: BTreeSet::new(),
                        present: BTreeSet::new(),
                        snapshot: Snapshot::default(),
                    });
                }
                Err(error) => {
                    view.phase = Phase::Retrying;
                    view.error = Some(error);
                    retry_at = Instant::now() + RETRY;
                }
            }
        }
        if let Some(e) = engine.as_mut() {
            let alive = e.graph.maintain().and_then(|ok| if ok { e.observe() } else { Ok(()) });
            match alive {
                Ok(()) => {
                    e.settle();
                    e.view(&mut view);
                    if e.pinned != pinned || e.released != released {
                        // Remembered as saved only once it is (tried again next pass).
                        match store.save(&e.pinned, &e.released) {
                            Ok(()) => {
                                pinned = e.pinned.clone();
                                released = e.released.clone();
                            }
                            Err(error) => view.message = Some(format!("Could not save placements: {error}")),
                        }
                    }
                }
                Err(error) => {
                    eprintln!("Wavelane lost PipeWire: {error}");
                    engine = None;
                    view = View { phase: Phase::Retrying, error: Some(error), ..Default::default() };
                    retry_at = Instant::now() + RETRY;
                }
            }
        }
        publish(&view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn store_round_trips_and_ignores_bad_lines() {
        let pinned = BTreeMap::from([("Spotify".to_string(), "alsa_output.x".to_string())]);
        let released = BTreeSet::from(["Discord".to_string()]);
        assert_eq!(parse_store(&format_store(&pinned, &released)), (pinned, released));
        assert!(parse_store("other=1\ndevice\t41\t41").0.is_empty());
        let (p, r) = parse_store("wavelane-placements=1\ndevice\tzz\t41\nwhat\t1\t41\nreleased\t-\t42\n");
        assert!(p.is_empty());
        assert_eq!(r, BTreeSet::from(["B".to_string()]));
    }
    #[test]
    fn applications_group_streams_and_agree_on_their_output() {
        let s = |node: u32, app: &str, on: &str| Stream {
            node,
            serial: node as u64,
            app: app.into(),
            playing_on: on.into(),
            info: true,
            movable: true,
            ..Default::default()
        };
        let apps = applications(
            &[s(1, "Firefox", "spk"), s(2, "Firefox", "spk"), s(3, "Game", "spk"), s(4, "Game", "hdmi")],
            &BTreeMap::from([("Firefox".to_string(), "spk".to_string())]),
        );
        assert_eq!(apps[0].playing_on.as_deref(), Some("spk"));
        assert_eq!(apps[0].pinned.as_deref(), Some("spk"));
        assert_eq!(apps[1].playing_on, None);
        assert_eq!(apps[1].streams, 2);
    }

    #[test]
    fn view_changes_include_errors_and_device_identity_but_not_live_levels() {
        let original = View { outputs: vec![Output::default()], ..Default::default() };
        let mut changed = original.clone();
        changed.outputs[0].level = Some(0.5);
        assert!(!original.differs(&changed));
        changed.outputs[0].device_description = "New mixer".into();
        assert!(original.differs(&changed));
        changed = original.clone();
        changed.outputs[0].node = 123;
        assert!(original.differs(&changed));
        changed = original.clone();
        changed.message = Some("Routing failed".into());
        assert!(original.differs(&changed));
        changed = original.clone();
        changed.error = Some("Disconnected".into());
        assert!(original.differs(&changed));
    }

    /// Live routing on this machine's PipeWire with silent test streams
    /// ("WLTest"). Needs two outputs: WL_A and WL_B (node names; B is the
    /// current default). "Everything" is left out: it would move every
    /// application on the machine. Run with
    /// `cargo test --release live_routing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_routing() {
        use std::process::{Child, Command as Run, Stdio};
        let (a, b) = (std::env::var("WL_A").expect("WL_A"), std::env::var("WL_B").expect("WL_B"));
        let config = std::env::temp_dir().join(format!("wavelane-live-{}", std::process::id()));
        // SAFETY: single test thread; set before the worker starts.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };
        let play = || -> Child {
            Run::new("pw-cat")
                .args(["-p", "-a", "--format", "s16", "--rate", "48000", "--channels", "2"])
                .args(["-P", "{ application.name = \"WLTest\" }", "/dev/zero"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("pw-cat")
        };
        let mut w = Worker::spawn();
        // Waits until the test app looks as expected.
        let until = |w: &Worker, what: &str, ok: &dyn Fn(Option<&App>, &View) -> bool| {
            let start = Instant::now();
            loop {
                let view = w.view();
                if ok(view.apps.iter().find(|x| x.key == "WLTest"), &view) {
                    println!("ok   {what} ({} ms)", start.elapsed().as_millis());
                    return;
                }
                if start.elapsed() > Duration::from_secs(6) {
                    let app = view.apps.iter().find(|x| x.key == "WLTest").cloned();
                    panic!("FAIL {what}: {app:?}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        };
        let on = |o: &str| {
            let o = o.to_owned();
            move |x: Option<&App>, _: &View| x.is_some_and(|x| x.playing_on.as_deref() == Some(o.as_str()))
        };
        let gone = |x: Option<&App>, _: &View| x.is_none();

        let mut p1 = play();
        until(&w, "stream appears on the default", &on(&b));
        w.send(Command::Place("WLTest".into(), Some(a.clone()))).unwrap();
        until(&w, "placed on A", &on(&a));
        thread::sleep(Duration::from_secs(3));
        until(&w, "still on A after 3 s (no one moves it back)", &on(&a));
        let mut p2 = play();
        until(&w, "second stream joins it on A", &|x, v| on(&a)(x, v) && x.is_some_and(|x| x.streams == 2));
        let _ = (p1.kill(), p2.kill(), p1.wait(), p2.wait());
        until(&w, "streams closed", &gone);
        let mut p3 = play();
        until(&w, "a new stream starts on A by itself", &on(&a));
        w.send(Command::Place("WLTest".into(), None)).unwrap();
        until(&w, "follows the default again", &|x, v| on(&b)(x, v) && x.is_some_and(|x| x.pinned.is_none()));
        // Placed while closed, then released while closed.
        w.send(Command::Place("WLTest".into(), Some(a.clone()))).unwrap();
        until(&w, "placed on A", &on(&a));
        let _ = (p3.kill(), p3.wait());
        until(&w, "closed", &gone);
        w.send(Command::Place("WLTest".into(), None)).unwrap();
        thread::sleep(Duration::from_millis(500));
        let mut p4 = play();
        until(&w, "released while closed: comes back on the default, not A", &on(&b));
        thread::sleep(Duration::from_secs(2));
        until(&w, "and stays there", &on(&b));
        // Volume on the desktop's cubic scale.
        w.send(Command::AppLevel { key: "WLTest".into(), volume: Some(0.5), mute: None }).unwrap();
        until(&w, "volume reads back 50%", &|x, _| x.and_then(|x| x.volume).is_some_and(|v| (v - 0.5).abs() < 0.01));
        let node = w.view().apps.iter().find(|x| x.key == "WLTest").unwrap().nodes[0];
        let dump = Run::new("pw-dump").arg(node.to_string()).output().unwrap().stdout;
        let dump: serde_json::Value = serde_json::from_slice(&dump).unwrap();
        let linear = dump[0]["info"]["params"]["Props"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|p| p["channelVolumes"][0].as_f64())
            .unwrap();
        assert!((linear - 0.125).abs() < 0.005, "channelVolumes {linear}");
        println!("ok   PipeWire holds 0.125 (cubic), as pavucontrol shows 50%");
        // Placements survive a restart of the engine.
        w.send(Command::Place("WLTest".into(), Some(a.clone()))).unwrap();
        until(&w, "placed on A", &on(&a));
        w.stop();
        let w = Worker::spawn();
        until(&w, "after an engine restart: still placed on A", &|x, v| on(&a)(x, v) && x.is_some_and(|x| x.pinned.as_deref() == Some(a.as_str())));
        w.send(Command::Place("WLTest".into(), None)).unwrap();
        until(&w, "back on the default", &on(&b));
        let _ = (p4.kill(), p4.wait());
        let _ = fs::remove_dir_all(&config);
    }
}
