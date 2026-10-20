//! The desktop audio graph (native/audio_graph.c): outputs, application
//! streams and their live levels, per-application routing and volume.
use std::ffi::{c_char, c_int, c_void, CString};

unsafe extern "C" {
    fn ag_open() -> *mut c_void;
    fn ag_close(graph: *mut c_void);
    fn ag_set_metering(graph: *mut c_void, on: c_int);
    fn ag_maintain(graph: *mut c_void) -> c_int;
    fn ag_snapshot(graph: *mut c_void, out: *mut c_char, size: usize) -> c_int;
    fn ag_levels(graph: *mut c_void, out: *mut c_char, size: usize) -> c_int;
    fn ag_route(graph: *mut c_void, node: u32, serial: u64, output: *const c_char) -> c_int;
    fn ag_set_level(graph: *mut c_void, node: u32, volume: f32, mute: c_int) -> c_int;
    fn ag_set_default_output(graph: *mut c_void, name: *const c_char) -> c_int;
}

/// An output a stream can play to (an `Audio/Sink`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    pub node: u32,
    pub serial: u64,
    /// `node.name`: what routing and the default refer to.
    pub name: String,
    pub description: String,
    pub nick: String,
    pub icon: String,
    /// `device.form-factor`: speaker, headphone, headset, hdmi...
    pub form_factor: String,
    /// The device it belongs to (0 for virtual outputs); outputs that share
    /// one are channels of the same hardware (a GoXLR's System, Game...).
    pub device: u64,
    pub device_description: String,
    pub volume: Option<f32>,
    pub mute: bool,
    pub level: Option<f32>,
    /// `node.group`: virtual outputs made together (one app's channels).
    pub group: String,
}

/// One application output stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stream {
    pub node: u32,
    pub serial: u64,
    pub pid: u64,
    pub running: bool,
    pub movable: bool,
    /// Application identity has arrived.
    pub info: bool,
    pub app: String,
    pub binary: String,
    pub media: String,
    pub node_name: String,
    pub icon: String,
    /// The output it plays on now (node.name), if linked.
    pub playing_on: String,
    /// The output it is explicitly pointed at (node.name), if any.
    pub targeted_at: String,
    pub volume: Option<f32>,
    pub mute: bool,
    pub level: Option<f32>,
}
impl Stream {
    /// Per-application identity, matching the session manager's own
    /// per-application stream memory where possible. Engine names that many
    /// applications share ("Chromium" for every Electron app, the WebRTC
    /// voice engine) fall through to the process.
    pub fn app_key(&self) -> String {
        let app = specific_app_name(&self.app);
        let key = [app.as_deref(), Some(self.binary.as_str()), Some(self.node_name.as_str())]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(|s| s.strip_suffix(".exe").filter(|s| !s.is_empty()).unwrap_or(s).to_owned());
        key.unwrap_or_else(|| format!("pid {}", self.pid))
    }
}

/// The application's own name, or None when it is only the audio engine's.
/// ALSA bridges name the program in brackets ("ALSA plug-in [game]").
fn specific_app_name(name: &str) -> Option<String> {
    let name = name.trim();
    for bridge in ["ALSA plug-in [", "PipeWire ALSA ["] {
        if let Some(inner) = name.strip_prefix(bridge).and_then(|s| s.strip_suffix(']')) {
            return Some(inner.to_owned()).filter(|s| !s.trim().is_empty());
        }
    }
    const ENGINES: [&str; 4] = ["chromium", "chromium input", "electron", "webrtc voiceengine"];
    (!name.is_empty() && !ENGINES.contains(&name.to_lowercase().as_str())).then(|| name.to_owned())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub healthy: bool,
    pub metadata: bool,
    pub error: String,
    /// The output in use and the one the user chose (node.name).
    pub default_output: Option<String>,
    pub configured_output: Option<String>,
    pub outputs: Vec<Output>,
    pub streams: Vec<Stream>,
}

fn unhex(s: &str) -> Result<String, String> {
    if s.len() % 2 != 0 {
        return Err("Odd hex field.".into());
    }
    let bytes = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "Invalid hex field.".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
fn int<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("Invalid number {s:?}."))
}
fn level(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.)
}

pub fn parse(raw: &str) -> Result<Snapshot, String> {
    let mut snapshot = Snapshot::default();
    let mut seen = false;
    for line in raw.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "graph" if f.len() == 4 => {
                seen = true;
                snapshot.healthy = f[1] == "1";
                snapshot.metadata = f[2] == "1";
                snapshot.error = unhex(f[3])?;
            }
            "default" if f.len() == 3 => {
                snapshot.default_output = Some(unhex(f[1])?).filter(|s| !s.is_empty());
                snapshot.configured_output = Some(unhex(f[2])?).filter(|s| !s.is_empty());
            }
            "output" if f.len() == 14 => snapshot.outputs.push(Output {
                node: int(f[1])?,
                serial: int(f[2])?,
                name: unhex(f[3])?,
                description: unhex(f[4])?,
                nick: unhex(f[5])?,
                icon: unhex(f[6])?,
                form_factor: unhex(f[7])?,
                device: int(f[8])?,
                device_description: unhex(f[9])?,
                volume: level(f[10]),
                mute: f[11] == "1",
                level: level(f[12]),
                group: unhex(f[13])?,
            }),
            "stream" if f.len() == 17 => snapshot.streams.push(Stream {
                node: int(f[1])?,
                serial: int(f[2])?,
                pid: int(f[3])?,
                running: f[4] == "3",
                movable: f[5] != "1",
                info: f[6] == "1",
                app: unhex(f[7])?,
                binary: unhex(f[8])?,
                media: unhex(f[9])?,
                node_name: unhex(f[10])?,
                icon: unhex(f[11])?,
                playing_on: unhex(f[12])?,
                targeted_at: unhex(f[13])?,
                volume: level(f[14]),
                mute: f[15] == "1",
                level: level(f[16]),
            }),
            other => return Err(format!("Unexpected graph record {other:?}.")),
        }
    }
    if !seen {
        return Err("Graph status absent.".into());
    }
    Ok(snapshot)
}

/// Owner of the native graph. Every call is safe from any thread (the
/// native side locks its loop).
pub struct AudioGraph {
    raw: *mut c_void,
    buffer: Vec<u8>,
}
// The native graph serialises every entry point on its own loop lock.
unsafe impl Send for AudioGraph {}

impl AudioGraph {
    pub fn open() -> Result<Self, String> {
        let raw = unsafe { ag_open() };
        if raw.is_null() {
            return Err("Could not start the PipeWire connection.".into());
        }
        Ok(Self { raw, buffer: vec![0; 1 << 18] })
    }
    /// Live levels on or off (meters exist only while something shows them).
    pub fn set_metering(&mut self, on: bool) {
        unsafe { ag_set_metering(self.raw, on as c_int) }
    }
    /// Periodic pass: Ok(true) healthy, Ok(false) a transient stall,
    /// Err when the connection is gone.
    pub fn maintain(&mut self) -> Result<bool, String> {
        match unsafe { ag_maintain(self.raw) } {
            0 => Ok(true),
            -2 => Ok(false),
            _ => Err("PipeWire connection lost.".into()),
        }
    }
    fn text(&mut self, fill: impl Fn(*mut c_char, usize) -> c_int) -> Result<String, String> {
        loop {
            match fill(self.buffer.as_mut_ptr().cast(), self.buffer.len()) {
                0 => break,
                -2 if self.buffer.len() < 1 << 24 => {
                    let size = self.buffer.len() * 2;
                    self.buffer.resize(size, 0);
                }
                _ => return Err("Graph snapshot failed.".into()),
            }
        }
        let len = self.buffer.iter().position(|b| *b == 0).ok_or("Unterminated snapshot.")?;
        Ok(String::from_utf8_lossy(&self.buffer[..len]).into_owned())
    }
    pub fn snapshot(&mut self) -> Result<Snapshot, String> {
        let raw = self.raw;
        parse(&self.text(|out, size| unsafe { ag_snapshot(raw, out, size) })?)
    }
    /// Current levels by node id (cheap; for redrawing at frame rate).
    pub fn levels(&mut self) -> Vec<(u32, f32)> {
        let raw = self.raw;
        let Ok(text) = self.text(|out, size| unsafe { ag_levels(raw, out, size) }) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| {
                let (node, level) = l.split_once('\t')?;
                Some((node.parse().ok()?, level.parse::<f32>().ok().filter(|v| *v >= 0.)?))
            })
            .collect()
    }
    /// Point a stream at an output (node.name), or `None` to follow the
    /// default output.
    pub fn route(&mut self, stream: &Stream, output: Option<&str>) -> Result<(), String> {
        let name = output.map(CString::new).transpose().map_err(|_| "Invalid output name.")?;
        let ptr = name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
        if unsafe { ag_route(self.raw, stream.node, stream.serial, ptr) } != 0 {
            return Err(format!("Could not move {}.", stream.app_key()));
        }
        Ok(())
    }
    /// Volume (linear, up to 1.5) and/or mute of a stream or an output.
    pub fn set_level(&mut self, node: u32, volume: Option<f32>, mute: Option<bool>) -> Result<(), String> {
        let volume = match volume {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.5),
            Some(_) => return Err("Invalid level.".into()),
            None => -1.0,
        };
        if unsafe { ag_set_level(self.raw, node, volume, mute.map_or(-1, c_int::from)) } != 0 {
            return Err("That is no longer playing.".into());
        }
        Ok(())
    }
    /// Make an output (node.name) the desktop's default, like `wpctl set-default`.
    pub fn set_default_output(&mut self, name: &str) -> Result<(), String> {
        let name = CString::new(name).map_err(|_| "Invalid output name.")?;
        if unsafe { ag_set_default_output(self.raw, name.as_ptr()) } != 0 {
            return Err("Could not switch the sound output.".into());
        }
        Ok(())
    }
}
impl Drop for AudioGraph {
    fn drop(&mut self) {
        unsafe { ag_close(self.raw) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(s: &str) -> String {
        s.bytes().map(|b| format!("{b:02x}")).collect()
    }
    #[test]
    fn parses_outputs_streams_and_defaults() {
        let raw = format!(
            "graph\t1\t1\t\ndefault\t{spk}\t{spk}\noutput\t86\t900\t{spk}\t{desc}\t{nick}\t{icon}\t{ff}\t52\t{dd}\t0.800000\t0\t0.250000\t\n\
             stream\t104\t3345\t1848\t3\t0\t1\t{app}\t{bin}\t{media}\t{node}\t{sicon}\t{spk}\t\t1.000000\t1\t-1.000000\n",
            spk = hex("alsa_output.pci.analog-stereo"),
            desc = hex("Speakers"),
            nick = hex("Speakers"),
            icon = hex("audio-card-analog"),
            ff = hex("internal"),
            dd = hex("Family 17h HD Audio"),
            app = hex("Firefox"),
            bin = hex("firefox"),
            media = hex("AudioStream"),
            node = hex("Firefox"),
            sicon = hex("firefox"),
        );
        let s = parse(&raw).unwrap();
        assert!(s.healthy && s.metadata);
        assert_eq!(s.default_output.as_deref(), Some("alsa_output.pci.analog-stereo"));
        let o = &s.outputs[0];
        assert_eq!((o.node, o.device, o.volume, o.level), (86, 52, Some(0.8), Some(0.25)));
        let st = &s.streams[0];
        assert!(st.running && st.movable && st.info && st.mute);
        assert_eq!(st.app_key(), "Firefox");
        assert_eq!(st.playing_on, "alsa_output.pci.analog-stereo");
        assert_eq!((st.targeted_at.as_str(), st.level), ("", None));
    }
    #[test]
    fn engine_names_fall_through_to_the_process() {
        let stream = |app: &str, binary: &str| Stream {
            app: app.into(),
            binary: binary.into(),
            node_name: "node".into(),
            ..Default::default()
        };
        assert_eq!(stream("Chromium", "Discord").app_key(), "Discord");
        assert_eq!(stream("WEBRTC VoiceEngine", "firefox").app_key(), "firefox");
        assert_eq!(stream("PipeWire ALSA [Game.exe]", "wine64").app_key(), "Game");
        assert_eq!(stream("Spotify", "spotify").app_key(), "Spotify");
    }
}
