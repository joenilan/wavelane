//! The map: every application that plays sound as a puck orbiting the output
//! it plays on, moved by drag and drop. A device with several outputs (a
//! GoXLR, BEACN Studio channels through LinX) is a hub with one port per
//! output; every other output is a planet. The map opens over the whole screen on
//! the overlay layer, with the desktop dimmed behind it, so a shortcut can
//! bring it up for a quick move. "Minimal" shows the same routing as lanes.
use crate::app::{key_id, Core};
use crate::engine::{App as MapApp, Command, Phase, View};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The map is laid out on a 1280x800 plane, scaled to fit the screen.
const PLANE: (f32, f32) = (1280., 800.);
const HUB: P = P(450., 440.);
const PORT_R: f32 = 42.;
const ORB: P = P(800., 725.);
/// The orb's place when there is no hub (planets fill the middle).
const ORB_ALONE: P = P(1185., 735.);
/// Hub ports in order, colors cycling.
const PORT_COLORS: [u32; 8] = [0xa6d5bd, 0xa6bfe7, 0xc1a9d8, 0xd9b68b, 0xe5a5c4, 0xc8d98b, 0xb4b9f0, 0xef9f76];
/// "Follow default" area (orbit view).
const LOOSE: (f32, f32, f32, f32) = (30., 130., 140., 640.);
/// How long a move shows as made before PipeWire confirms it.
const SETTLE: Duration = Duration::from_millis(2500);
/// Fade in and out, like Plasma's own overlays.
const FADE: Duration = Duration::from_millis(650);
/// How dark the tint over the blurred desktop gets.
const DIM: f32 = 0.32;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct P(f32, f32);
impl P {
    fn dist(self, o: P) -> f32 {
        ((self.0 - o.0).powi(2) + (self.1 - o.1).powi(2)).sqrt()
    }
    fn lerp(self, o: P, t: f32) -> P {
        P(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t)
    }
}

/// Where an application plays, as the map shows it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Spot {
    Port(u8),
    Device(String),
    Loose,
    /// A hub as a whole: its first port.
    Studio,
}

#[derive(Clone)]
enum Shape {
    Circle(P, f32),
    Rect(P, P),
}

/// A place to drop applications.
#[derive(Clone)]
struct Zone {
    spot: Spot,
    shape: Shape,
    label: String,
    color: u32,
    icon: &'static str,
    /// A device hidden from the map (shown while editing devices).
    hidden: bool,
    /// A device in use (the default output, or apps placed on it).
    in_use: bool,
}
impl Zone {
    fn center(&self) -> P {
        match self.shape {
            Shape::Circle(c, _) => c,
            Shape::Rect(a, b) => P((a.0 + b.0) / 2., (a.1 + b.1) / 2.),
        }
    }
    fn hit(&self, p: P) -> bool {
        match self.shape {
            Shape::Circle(c, r) => p.dist(c) < r + 34.,
            Shape::Rect(a, b) => p.0 >= a.0 && p.0 <= b.0 && p.1 >= a.1 && p.1 <= b.1,
        }
    }
}

enum Grab {
    Apps(Vec<String>),
    Everything,
}

/// Each moved application's placement before a move, and the sound output
/// before an Everything move.
struct Undo {
    apps: Vec<(String, Option<String>)>,
    default: Option<String>,
}

/// A device with several outputs, drawn as a hub with one port each.
#[derive(Clone, Debug, PartialEq)]
struct Hub {
    label: String,
    /// (node.name, short label) per port, in order.
    ports: Vec<(String, String)>,
}

/// The hub on this desktop: LinX's channels if LinX runs, else the
/// device with the most outputs (displays excluded: a monitor's ports are
/// not a mixer's channels).
fn find_hub(outputs: &[crate::graph::Output]) -> Option<Hub> {
    let mut linx: Vec<&crate::graph::Output> =
        outputs.iter().filter(|o| o.name.starts_with("linx_link_in_")).collect();
    if linx.len() >= 2 {
        linx.sort_by(|a, b| a.name.cmp(&b.name));
        return Some(Hub {
            label: "BEACN Studio".into(),
            ports: linx
                .iter()
                .map(|o| (o.name.clone(), format!("Link In {}", o.name.trim_start_matches("linx_link_in_"))))
                .collect(),
        });
    }
    let display = |o: &crate::graph::Output| {
        let text = format!("{} {} {}", o.name, o.description, o.form_factor).to_lowercase();
        ["hdmi", "displayport", "iec958"].iter().any(|w| text.contains(w))
    };
    let mut groups: BTreeMap<u64, Vec<&crate::graph::Output>> = BTreeMap::new();
    for o in outputs.iter().filter(|o| o.device != 0 && !display(o)) {
        groups.entry(o.device).or_default().push(o);
    }
    let (_, mut group) = groups.into_iter().filter(|(_, g)| g.len() >= 2).max_by_key(|(_, g)| g.len())?;
    group.truncate(8);
    let device = group[0].device_description.clone();
    Some(Hub {
        label: if device.is_empty() { friendly(&group[0].description) } else { device.clone() },
        ports: group
            .iter()
            .map(|o| {
                let short = o.description.strip_prefix(&device).map(str::trim).filter(|s| !s.is_empty());
                (o.name.clone(), short.map_or_else(|| friendly(&o.description), str::to_owned))
            })
            .collect(),
    })
}

/// Port centers around the hub: four at its corners in reading order (like
/// LinX), otherwise evenly round.
fn port_positions(n: usize) -> Vec<P> {
    if n == 4 {
        return vec![P(290., 280.), P(610., 280.), P(290., 610.), P(610., 610.)];
    }
    let start: f32 = match n {
        2 => 180f32,
        4 => -135.,
        _ => -90.,
    };
    (0..n)
        .map(|i| {
            let a = (start + i as f32 * 360. / n as f32).to_radians();
            P(HUB.0 + a.cos() * 226., HUB.1 + a.sin() * 226.)
        })
        .collect()
}
struct Drag {
    grab: Grab,
    start: P,
    at: P,
    moved: bool,
}

pub struct SoundMap {
    view: View,
    viewed: Instant,
    minimal: bool,
    /// Every device is shown, with Hide/Show.
    edit: bool,
    /// The puck under the pointer (its name shows in crowded orbits).
    hovered: Option<String>,
    /// Displayed puck centers, easing toward their slots.
    shown: BTreeMap<String, P>,
    selected: BTreeSet<String>,
    drag: Option<Drag>,
    lasso: Option<(P, P)>,
    /// A press on empty space that may become a click (closes the map).
    pressed_empty: bool,
    /// Moves sent but not yet seen in the graph.
    pending: BTreeMap<String, (Spot, Instant)>,
    toast: Option<(String, Instant)>,
    /// The toast offers Undo for the move it reports.
    toast_undo: bool,
    /// How to put back the last move.
    undo: Option<Undo>,
    /// The application the keyboard is on (arrow keys).
    focused: Option<String>,
    /// Apps in reading order, for the arrow keys (from the last frame).
    order: Vec<String>,
    /// Volume just set by scrolling, shown until PipeWire echoes it.
    volume_edit: Option<(String, f32, Instant)>,
    /// Mute just toggled (middle click), shown until PipeWire echoes it.
    mute_edit: Option<(String, bool, Instant)>,
    hub: Option<Hub>,
    /// Smoothed live levels by output name and by application key.
    out_levels: BTreeMap<String, f32>,
    app_levels: BTreeMap<String, f32>,
    started: Instant,
    /// Fading out; the window closes when done.
    closing: Option<Instant>,
    /// The next frame while nothing moves.
    tick: Option<Task<()>>,
    /// Scale and origin of the plane in the window, from the last frame.
    frame: (f32, f32, f32),
    focus: FocusHandle,
}

fn alpha(color: u32, a: f32) -> Hsla {
    let mut c: Hsla = rgb(color).into();
    c.a = a;
    c
}

const TEXT: u32 = 0xe5e7e8;
const MUTED: u32 = 0xa0a6ad;
const LOOSE_COLOR: u32 = 0x9aa1a8;
/// Colors for devices on this PC, in list order.
const DEVICE_COLORS: [u32; 5] = [0xef9f76, 0x8fd3e0, 0xe5a5c4, 0xc8d98b, 0xb4b9f0];

/// A short device name for the map: "GA102 High Definition Audio Controller
/// Digital Stereo (HDMI 2) [BenQ GL2760]" becomes "BenQ GL2760 · HDMI 2".
pub(crate) fn friendly(description: &str) -> String {
    let inner = |open: char, close: char| -> Option<&str> {
        let start = description.rfind(open)?;
        let end = description[start..].find(close)? + start;
        Some(description[start + 1..end].trim()).filter(|s| !s.is_empty())
    };
    let port = inner('(', ')').filter(|p| p.len() <= 24);
    match (inner('[', ']'), port) {
        (Some(monitor), Some(port)) => format!("{monitor} · {port}"),
        (Some(monitor), None) => monitor.to_owned(),
        _ => description.to_owned(),
    }
}

fn device_icon(name: &str, description: &str) -> &'static str {
    let text = format!("{name} {description}").to_lowercase();
    if ["hdmi", "displayport", "monitor"].iter().any(|w| text.contains(w)) {
        "console/monitor.svg"
    } else if ["headphone", "headset", "earbud", "airpods"].iter().any(|w| text.contains(w)) {
        "console/headphones.svg"
    } else {
        "console/speaker.svg"
    }
}

impl SoundMap {
    pub fn new(minimal: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            view: cx.global::<Core>().worker.view(),
            viewed: Instant::now(),
            minimal,
            edit: false,
            hovered: None,
            shown: BTreeMap::new(),
            selected: BTreeSet::new(),
            drag: None,
            lasso: None,
            pressed_empty: false,
            pending: BTreeMap::new(),
            toast: None,
            toast_undo: false,
            undo: None,
            focused: None,
            order: Vec::new(),
            volume_edit: None,
            mute_edit: None,
            hub: None,
            out_levels: BTreeMap::new(),
            app_levels: BTreeMap::new(),
            started: Instant::now(),
            closing: None,
            tick: None,
            frame: (1., 0., 0.),
            focus,
        }
    }

    fn port_colors(&self) -> Vec<u32> {
        let n = self.hub.as_ref().map_or(0, |h| h.ports.len());
        (0..n).map(|i| PORT_COLORS[i % PORT_COLORS.len()]).collect()
    }

    /// Where an output is on the map.
    fn spot_for(&self, output: &str) -> Spot {
        match self.hub.as_ref().and_then(|h| h.ports.iter().position(|(n, _)| n == output)) {
            Some(i) => Spot::Port(i as u8 + 1),
            None => Spot::Device(output.to_owned()),
        }
    }

    /// The output a drop target means (None: follow the default).
    fn output_of(&self, spot: &Spot) -> Option<String> {
        match spot {
            Spot::Port(n) => self.hub.as_ref().and_then(|h| h.ports.get(*n as usize - 1)).map(|(n, _)| n.clone()),
            Spot::Studio => self.hub.as_ref().and_then(|h| h.ports.first()).map(|(n, _)| n.clone()),
            Spot::Device(name) => Some(name.clone()),
            Spot::Loose => None,
        }
    }

    /// Window position to plane position.
    fn plane(&self, at: Point<Pixels>) -> P {
        let (s, ox, oy) = self.frame;
        P((f32::from(at.x) - ox) / s, (f32::from(at.y) - oy) / s)
    }

    /// Where an application shows: the output it plays on (its placement,
    /// else the default it follows).
    fn spot_of(&self, app: &MapApp) -> Spot {
        if let Some((spot, at)) = self.pending.get(&app.key) {
            if at.elapsed() < SETTLE {
                return spot.clone();
            }
        }
        app.playing_on
            .clone()
            .or_else(|| app.pinned.clone())
            .or_else(|| self.view.default_output.clone())
            .map_or(Spot::Loose, |o| self.spot_for(&o))
    }

    /// Devices on this PC shown on the map: not hidden, or in use, or all of
    /// them while editing. (node.name, description, hidden, in use)
    fn devices(&self, cx: &App) -> Vec<(String, String, bool, bool)> {
        let hidden = &cx.global::<Core>().hidden_outputs;
        let mut used: BTreeSet<String> = self
            .view
            .apps
            .iter()
            .filter_map(|a| match self.spot_of(a) {
                Spot::Device(d) => Some(d),
                _ => None,
            })
            .collect();
        used.extend(self.view.default_output.clone());
        self.view
            .outputs
            .iter()
            .filter(|o| !self.hub.as_ref().is_some_and(|h| h.ports.iter().any(|(n, _)| n == &o.name)))
            .map(|o| (o.name.clone(), o.description.clone(), hidden.contains(&o.name), used.contains(&o.name)))
            .filter(|(_, _, hidden, in_use)| self.edit || !hidden || *in_use)
            .collect()
    }

    fn zones(&self, cx: &App) -> Vec<Zone> {
        let colors = self.port_colors();
        let ports = colors.len();
        let labels: Vec<String> = self.hub.as_ref().map_or_else(Vec::new, |h| h.ports.iter().map(|(_, l)| l.clone()).collect());
        let devices = self.devices(cx);
        let zone = |spot: Spot, shape: Shape, label: String, color: u32| Zone {
            spot,
            shape,
            label,
            color,
            icon: "",
            hidden: false,
            in_use: false,
        };
        let mut zones = Vec::new();
        if self.minimal {
            // Slimmer hub lanes when many devices need the room.
            let lane = if devices.len() > 4 || ports > 4 { (580. / ports.max(1) as f32).min(146.) } else { 172. };
            let studio_end = if ports > 0 { 32. + ports as f32 * lane + (ports as f32 - 1.) * 12. } else { -8. };
            for c in 0..ports {
                let x = 32. + c as f32 * (lane + 12.);
                zones.push(zone(
                    Spot::Port(c as u8 + 1),
                    Shape::Rect(P(x, 156.), P(x + lane, 626.)),
                    labels[c].clone(),
                    colors[c],
                ));
            }
            // Up to four lanes a row; more devices take more rows.
            let n = devices.len().max(1);
            let rows = n.div_ceil(4);
            let per_row = n.div_ceil(rows) as f32;
            let left = studio_end + 40.;
            let w = ((1248. - left) - 12. * (per_row - 1.)) / per_row;
            let h = (470. - 16. * (rows as f32 - 1.)) / rows as f32;
            for (i, (name, description, hidden, in_use)) in devices.iter().enumerate() {
                let (col, row) = ((i % per_row as usize) as f32, (i / per_row as usize) as f32);
                let (x, y) = (left + col * (w + 12.), 156. + row * (h + 16.));
                zones.push(Zone {
                    icon: device_icon(name, description),
                    hidden: *hidden,
                    in_use: *in_use,
                    ..zone(
                        Spot::Device(name.clone()),
                        Shape::Rect(P(x, y), P(x + w, y + h)),
                        friendly(description),
                        DEVICE_COLORS[i % DEVICE_COLORS.len()],
                    )
                });
            }
            if let Some(hub) = &self.hub {
                zones.push(zone(Spot::Studio, Shape::Rect(P(32., 118.), P(studio_end, 150.)), hub.label.clone(), colors[0]));
            }
            zones.push(zone(
                Spot::Loose,
                Shape::Rect(P(32., 648.), P(1248., 768.)),
                "Follow default · wherever the default output goes".into(),
                LOOSE_COLOR,
            ));
        } else {
            for (c, at) in port_positions(ports).into_iter().enumerate() {
                zones.push(zone(Spot::Port(c as u8 + 1), Shape::Circle(at, PORT_R), labels[c].clone(), colors[c]));
            }
            // One column up to four devices, then two, then three; smaller
            // planets as more arrive.
            let n = devices.len();
            let alone = self.hub.is_none();
            // Beside a hub: one column up to four devices, then two, then
            // three. Alone: across the middle, up to four columns.
            let cols = if alone { n.div_ceil(3).clamp(1, 4) } else { n.div_ceil(4).clamp(1, 3) };
            let rows = n.div_ceil(cols).max(1);
            let r = if alone {
                (64. - 4. * (n as f32 - 4.).max(0.)).clamp(34., 64.)
            } else {
                (54. - 4. * (n as f32 - 3.).max(0.)).clamp(30., 54.)
            };
            // Editing adds a Hide/Show row under each name.
            let gap = ((if self.edit { 690. } else { 620. }) / rows as f32).min(230.);
            let xs: Vec<f32> = if alone {
                (0..cols).map(|c| if cols == 1 { 700. } else { 420. + c as f32 * 680. / (cols - 1) as f32 }).collect()
            } else {
                match cols {
                    1 => vec![1075.],
                    2 => vec![985., 1160.],
                    _ => vec![945., 1065., 1185.],
                }
            };
            for (i, (name, description, hidden, in_use)) in devices.iter().enumerate() {
                let (col, row) = (i % cols, i / cols);
                let y = 450. + (row as f32 - (rows as f32 - 1.) / 2.) * gap;
                zones.push(Zone {
                    icon: device_icon(name, description),
                    hidden: *hidden,
                    in_use: *in_use,
                    ..zone(
                        Spot::Device(name.clone()),
                        Shape::Circle(P(xs[col], y), r),
                        friendly(description),
                        DEVICE_COLORS[i % DEVICE_COLORS.len()],
                    )
                });
            }
            if let Some(hub) = &self.hub {
                zones.push(zone(
                    Spot::Studio,
                    Shape::Rect(P(HUB.0 - 100., HUB.1 - 55.), P(HUB.0 + 100., HUB.1 + 55.)),
                    hub.label.clone(),
                    colors[0],
                ));
            }
            let (x, y, w, h) = LOOSE;
            zones.push(zone(Spot::Loose, Shape::Rect(P(x, y), P(x + w, y + h)), "Follow default".into(), LOOSE_COLOR));
        }
        zones
    }

    /// Slot centers for every application, by zone. Crowded zones pack
    /// tighter; a lane that still cannot hold them all leaves the rest out
    /// (shown as "+N more").
    fn slots(&self, zones: &[Zone]) -> BTreeMap<String, P> {
        let mut groups: BTreeMap<Spot, Vec<&MapApp>> = BTreeMap::new();
        for app in &self.view.apps {
            let mut spot = self.spot_of(app);
            if !zones.iter().any(|z| z.spot == spot) {
                spot = Spot::Loose;
            }
            groups.entry(spot).or_default().push(app);
        }
        let mut out = BTreeMap::new();
        for zone in zones {
            let Some(apps) = groups.get(&zone.spot) else { continue };
            let n = apps.len();
            for (i, app) in apps.iter().enumerate() {
                let p = match (&zone.shape, self.minimal) {
                    (Shape::Rect(a, b), true) if zone.spot == Spot::Loose => {
                        let per = (((b.0 - a.0) - 32.) / 162.).max(1.) as usize;
                        let rows = n.div_ceil(per);
                        let step = ((b.1 - a.1 - 48.) / rows as f32).min(44.);
                        if step < 28. && i >= per * ((b.1 - a.1 - 48.) / 28.) as usize - 1 {
                            continue;
                        }
                        let step = step.max(28.);
                        P(a.0 + 91. + (i % per) as f32 * 162., a.1 + 62. + (i / per) as f32 * step)
                    }
                    (Shape::Rect(a, b), true) => {
                        // Room for Hide/Show under a device lane's name.
                        let top = if self.edit && matches!(zone.spot, Spot::Device(_)) { 28. } else { 0. };
                        let (step, fits) = lane_step(b.1 - a.1 - top, n);
                        if i >= fits {
                            continue;
                        }
                        P((a.0 + b.0) / 2., a.1 + top + 46. + step / 2. + i as f32 * step)
                    }
                    (Shape::Rect(a, b), false) => {
                        let cols = if n > 7 { 2 } else { 1 };
                        let rows = n.div_ceil(cols).max(1);
                        let step = ((b.1 - a.1 - 90.) / rows as f32).min(86.);
                        let x = (a.0 + b.0) / 2. + ((i % cols) as f32 - (cols as f32 - 1.) / 2.) * 62.;
                        P(x, a.1 + 50. + (i / cols) as f32 * step)
                    }
                    (Shape::Circle(c, r), _) if is_grid(zone) => {
                        // A device among many: a tight ring around it, so
                        // its apps stay clear of the devices next to it.
                        let size = zone_puck(zone, n);
                        let per = ((1.5 * std::f32::consts::PI * (r + size / 2. + 10.)) / (size + 6.)) as usize;
                        let (ring, j) = (i / per.max(1), i % per.max(1));
                        let in_ring = (n - ring * per).min(per) as f32;
                        let radius = r + size / 2. + 10. + ring as f32 * (size + 6.);
                        // The top three quarters: the name sits below.
                        let span = 1.5 * std::f32::consts::PI;
                        let a = -std::f32::consts::FRAC_PI_2 - span / 2. + (j as f32 + 0.5) / in_ring * span;
                        P(c.0 + a.cos() * radius, c.1 + a.sin() * radius)
                    }
                    (Shape::Circle(c, r), _) => {
                        let base = match zone.spot {
                            Spot::Port(_) => (c.1 - HUB.1).atan2(c.0 - HUB.0),
                            _ => std::f32::consts::PI,
                        };
                        // Ring k holds 5 + 3k pucks.
                        let size = puck_size(n);
                        let (mut ring, mut first) = (0, 0);
                        while i >= first + 5 + 3 * ring {
                            first += 5 + 3 * ring;
                            ring += 1;
                        }
                        let in_ring = (n - first).min(5 + 3 * ring) as f32;
                        let radius = r + 58. + ring as f32 * (size + 14.);
                        // Up to four, names show: spaced for a name each.
                        let step = if n <= 4 { (96. / radius).min(1.) } else { ((size + 16.) / radius).min(0.62) };
                        let a = base + ((i - first) as f32 - (in_ring - 1.) / 2.) * step;
                        P(c.0 + a.cos() * radius, c.1 + a.sin() * radius)
                    }
                };
                out.insert(app.key.clone(), p);
            }
        }
        out
    }

    /// Hide/Show for a device while editing. The device in use stays.
    fn visibility_pill(&self, zone: &Zone, left: Pixels, top: Pixels, width: Pixels, s: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let Spot::Device(name) = zone.spot.clone() else { unreachable!() };
        let locked = zone.in_use && !zone.hidden;
        div()
            .id(SharedString::from(format!("map-device-visibility-{}", key_id(&name))))
            .absolute()
            .left(left)
            .top(top)
            .w(width)
            .py(px(3. * s))
            .rounded(px(999.))
            .border_1()
            .border_color(rgb(0x404247))
            .bg(alpha(0x2d3035, 0.9))
            .text_center()
            .text_size(px(11. * s))
            .text_color(rgb(if locked { 0x747c82 } else { TEXT }))
            .child(if locked {
                "In use"
            } else if zone.hidden {
                "Show"
            } else {
                "Hide"
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(!locked, |pill| {
                pill.cursor_pointer().hover(|b| b.bg(rgb(0x363a40))).on_click(cx.listener(move |_, _, _, cx| {
                    let core = cx.global_mut::<Core>();
                    if !core.hidden_outputs.remove(&name) {
                        core.hidden_outputs.insert(name.clone());
                    }
                    crate::app::save_hidden(&core.hidden_outputs);
                    cx.notify();
                }))
            })
            .test_support()
    }

    fn zone_name(zones: &[Zone], spot: &Spot) -> String {
        zones
            .iter()
            .find(|z| &z.spot == spot)
            .map_or_else(|| "the default output".into(), |z| z.label.clone())
    }

    fn send(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        match cx.global::<Core>().worker.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.toast = Some((error, Instant::now()));
                self.toast_undo = false;
                false
            }
        }
    }

    fn remember_undo(&mut self, keys: &[String], everything: bool) {
        let apps = self
            .view
            .apps
            .iter()
            .filter(|a| everything || keys.contains(&a.key))
            .map(|a| (a.key.clone(), a.pinned.clone()))
            .collect();
        let default = everything.then(|| self.view.default_output.clone()).flatten();
        self.undo = Some(Undo { apps, default });
    }

    /// Put back the last move: the sound output (after Everything), then
    /// each application as it was.
    fn undo(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo.take() else {
            self.toast = Some(("Nothing to undo".into(), Instant::now()));
            self.toast_undo = false;
            return;
        };
        if let Some(default) = undo.default {
            self.send(Command::PlaceAll(Some(default)), cx);
        }
        for (key, pinned) in undo.apps {
            self.pending.remove(&key);
            self.send(Command::Place(key, pinned), cx);
        }
        self.toast = Some(("Undone".into(), Instant::now()));
        self.toast_undo = false;
        cx.notify();
    }

    fn drop_on(&mut self, grab: Grab, spot: Spot, zones: &[Zone], cx: &mut Context<Self>) {
        let spot = if spot == Spot::Studio { Spot::Port(1) } else { spot };
        let target = self.output_of(&spot);
        // Following the default, it shows on the default output.
        let shows = target
            .clone()
            .or_else(|| self.view.default_output.clone())
            .map_or(Spot::Loose, |o| self.spot_for(&o));
        let name = Self::zone_name(zones, &spot);
        match grab {
            Grab::Everything => {
                self.remember_undo(&[], true);
                if self.send(Command::PlaceAll(target), cx) {
                    for app in &self.view.apps {
                        self.pending.insert(app.key.clone(), (shows.clone(), Instant::now()));
                    }
                    self.toast = Some((format!("Everything → {name}"), Instant::now()));
                    self.toast_undo = true;
                }
            }
            Grab::Apps(keys) => {
                let moving: Vec<String> = keys
                    .into_iter()
                    .filter(|k| {
                        self.view
                            .apps
                            .iter()
                            .find(|a| &a.key == k)
                            .is_some_and(|a| self.spot_of(a) != spot)
                    })
                    .collect();
                if !moving.is_empty() {
                    self.remember_undo(&moving, false);
                }
                let mut moved = Vec::new();
                for key in moving {
                    if self.send(Command::Place(key.clone(), target.clone()), cx) {
                        self.pending.insert(key.clone(), (shows.clone(), Instant::now()));
                        moved.push(key);
                    }
                }
                let who = match moved.as_slice() {
                    [] => return,
                    [one] => self
                        .view
                        .apps
                        .iter()
                        .find(|a| &a.key == one)
                        .map_or_else(|| one.clone(), |a| a.name.clone()),
                    many => format!("{} apps", many.len()),
                };
                self.toast = Some((format!("{who} → {name}"), Instant::now()));
                self.toast_undo = true;
            }
        }
    }

    fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_none() {
            self.closing = Some(Instant::now());
            self.drag = None;
            self.lasso = None;
            cx.notify();
        }
    }

    /// 0 (hidden) to 1 (shown), eased.
    fn shown_amount(&self) -> f32 {
        let fade = FADE.as_secs_f32();
        let t = match self.closing {
            Some(at) => 1. - (at.elapsed().as_secs_f32() / fade).min(1.),
            None => (self.started.elapsed().as_secs_f32() / fade).min(1.),
        };
        1. - (1. - t).powi(3)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        let p = self.plane(event.position);
        self.selected.clear();
        self.lasso = Some((p, p));
        self.pressed_empty = true;
        cx.notify();
    }

    fn grab_app(&mut self, key: String, event: &MouseDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let p = self.plane(event.position);
        if event.modifiers.control || event.modifiers.shift {
            if !self.selected.remove(&key) {
                self.selected.insert(key.clone());
            }
        } else if !self.selected.contains(&key) {
            self.selected = [key.clone()].into();
        }
        let keys = if self.selected.contains(&key) {
            self.selected.iter().cloned().collect()
        } else {
            vec![key]
        };
        self.drag = Some(Drag { grab: Grab::Apps(keys), start: p, at: p, moved: false });
        cx.notify();
    }

    fn grab_everything(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let p = self.plane(event.position);
        self.selected.clear();
        self.drag = Some(Drag { grab: Grab::Everything, start: p, at: p, moved: false });
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let p = self.plane(event.position);
        if let Some(drag) = &mut self.drag {
            drag.at = p;
            drag.moved |= p.dist(drag.start) > 4.;
            cx.notify();
        } else if let Some((start, _)) = self.lasso {
            self.lasso = Some((start, p));
            if p.dist(start) > 4. {
                self.pressed_empty = false;
            }
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        let p = self.plane(event.position);
        let zones = self.zones(cx);
        if let Some(drag) = self.drag.take() {
            if drag.moved {
                if let Some(zone) = zones.iter().find(|z| z.hit(p)) {
                    let spot = zone.spot.clone();
                    self.drop_on(drag.grab, spot, &zones, cx);
                }
            }
        } else if let Some((a, b)) = self.lasso.take() {
            if self.pressed_empty && !zones.iter().any(|z| z.hit(p)) {
                // A click on empty space puts the map away.
                self.close(window, cx);
                return;
            }
            let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
            let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
            self.selected = self
                .shown
                .iter()
                .filter(|(_, p)| p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1)
                .map(|(k, _)| k.clone())
                .collect();
        }
        self.pressed_empty = false;
        cx.notify();
    }

    /// The map from the keyboard: arrows pick an application (Space adds it
    /// to the selection, A selects all), then 1-4 Link In, 5-9 devices, 0 Not
    /// linked, S back to the Studio. With Shift, the key moves everything.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let m = &event.keystroke.modifiers;
        let zones = self.zones(cx);
        let step = |this: &mut Self, forward: bool| {
            if this.order.is_empty() {
                return;
            }
            let at = this.focused.as_ref().and_then(|f| this.order.iter().position(|k| k == f));
            let next = match (at, forward) {
                (None, true) => 0,
                (None, false) => this.order.len() - 1,
                (Some(i), true) => (i + 1) % this.order.len(),
                (Some(i), false) => (i + this.order.len() - 1) % this.order.len(),
            };
            this.focused = Some(this.order[next].clone());
        };
        let target = match key {
            "escape" => return self.close(window, cx),
            "z" if m.control => return self.undo(cx),
            "right" | "down" => return { step(self, true); cx.notify() },
            "left" | "up" => return { step(self, false); cx.notify() },
            "tab" => return { step(self, !m.shift); cx.notify() },
            "space" => {
                if let Some(f) = self.focused.clone() {
                    if !self.selected.remove(&f) {
                        self.selected.insert(f);
                    }
                }
                return cx.notify();
            }
            "a" if !m.control => {
                self.selected = self.view.apps.iter().map(|a| a.key.clone()).collect();
                return cx.notify();
            }
            "0" | ")" | "n" => Some(Spot::Loose),
            k => {
                let digit = match k {
                    "!" => Some(1),
                    "@" => Some(2),
                    "#" => Some(3),
                    "$" => Some(4),
                    "%" => Some(5),
                    "^" => Some(6),
                    "&" => Some(7),
                    "*" => Some(8),
                    "(" => Some(9),
                    _ => k.parse::<usize>().ok(),
                };
                // Hub ports first, then devices.
                let ports = self.hub.as_ref().map_or(0, |h| h.ports.len());
                match digit {
                    Some(d) if (1..=ports).contains(&d) => Some(Spot::Port(d as u8)),
                    Some(d) if d >= 1 => zones
                        .iter()
                        .filter(|z| matches!(z.spot, Spot::Device(_)))
                        .nth(d - ports - 1)
                        .map(|z| z.spot.clone()),
                    _ => None,
                }
            }
        };
        let Some(spot) = target else { return };
        let everything = m.shift;
        let grab = if everything {
            Grab::Everything
        } else if !self.selected.is_empty() {
            Grab::Apps(self.selected.iter().cloned().collect())
        } else if let Some(f) = self.focused.clone() {
            Grab::Apps(vec![f])
        } else {
            self.toast = Some(("Pick an app with the arrow keys first".into(), Instant::now()));
            self.toast_undo = false;
            return cx.notify();
        };
        self.drop_on(grab, spot, &zones, cx);
        cx.notify();
    }

    /// Scroll on an application: its own volume, in 5% steps.
    fn scroll_volume(&mut self, key: String, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / 40.,
        };
        if lines == 0. {
            return;
        }
        let current = self.volume_of(&key);
        let volume = ((current + lines * 0.05) * 100.).round().clamp(0., 100.) / 100.;
        self.volume_edit = Some((key.clone(), volume, Instant::now()));
        self.send(Command::AppLevel { key, volume: Some(volume), mute: None }, cx);
        cx.notify();
    }

    /// Middle click on an application: mute or unmute it.
    fn toggle_mute(&mut self, key: String, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let mute = !self.muted(&key);
        self.mute_edit = Some((key.clone(), mute, Instant::now()));
        self.send(Command::AppLevel { key, volume: None, mute: Some(mute) }, cx);
        cx.notify();
    }

    /// An application's volume as shown: a just-set value, else PipeWire's.
    fn volume_of(&self, key: &str) -> f32 {
        if let Some((k, v, at)) = &self.volume_edit {
            if k == key && at.elapsed() < Duration::from_millis(1500) {
                return *v;
            }
        }
        self.view
            .apps
            .iter()
            .find(|a| a.key == key)
            .and_then(|a| a.volume)
            .unwrap_or(1.)
    }

    /// The volume of this application was just changed by scrolling.
    fn adjusting(&self, key: &str) -> bool {
        self.volume_edit
            .as_ref()
            .is_some_and(|(k, _, at)| k == key && at.elapsed() < Duration::from_millis(1500))
    }

    fn muted(&self, key: &str) -> bool {
        if let Some((k, m, at)) = &self.mute_edit {
            if k == key && at.elapsed() < Duration::from_millis(1500) {
                return *m;
            }
        }
        self.view.apps.iter().find(|a| a.key == key).is_some_and(|a| a.mute)
    }

    fn hover(&self, zones: &[Zone]) -> Option<Spot> {
        let drag = self.drag.as_ref().filter(|d| d.moved)?;
        zones.iter().find(|z| z.hit(drag.at)).map(|z| z.spot.clone())
    }

    fn refresh(&mut self, cx: &App) {
        if self.viewed.elapsed() >= Duration::from_millis(100) {
            self.view = cx.global::<Core>().worker.view();
            self.viewed = Instant::now();
            let apps = &self.view.apps;
            self.pending.retain(|key, (_, at)| at.elapsed() < SETTLE && apps.iter().any(|a| &a.key == key));
            self.selected.retain(|key| apps.iter().any(|a| &a.key == key));
        }
        let hub = find_hub(&self.view.outputs);
        if hub != self.hub {
            self.hub = hub;
        }
        // Live levels: fast attack, slow release.
        let worker = &cx.global::<Core>().worker;
        let smooth = |old: f32, peak: f32| if peak > old { peak } else { old * 0.9 + peak * 0.1 };
        for o in &self.view.outputs {
            let peak = worker.level(o.node).unwrap_or(0.).clamp(0., 1.);
            let old = self.out_levels.get(&o.name).copied().unwrap_or(0.);
            self.out_levels.insert(o.name.clone(), smooth(old, peak));
        }
        for a in &self.view.apps {
            let peak = a.nodes.iter().filter_map(|n| worker.level(*n)).fold(0f32, f32::max).clamp(0., 1.);
            let old = self.app_levels.get(&a.key).copied().unwrap_or(0.);
            self.app_levels.insert(a.key.clone(), smooth(old, peak));
        }
    }
}

/// Cubic bezier point for a cable that leaves `a` horizontally.
fn bezier(a: P, b: P, t: f32) -> P {
    let m = (a.0 + b.0) / 2.;
    let (c1, c2) = (P(m, a.1), P(m, b.1));
    let u = 1. - t;
    let w = [u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t];
    P(
        w[0] * a.0 + w[1] * c1.0 + w[2] * c2.0 + w[3] * b.0,
        w[0] * a.1 + w[1] * c1.1 + w[2] * c2.1 + w[3] * b.1,
    )
}

/// A cable to paint: from, to, color, opacity, flow speed (0 = still).
type Cable = (P, P, u32, f32, f32);
/// A volume ring: center, radius, level 0..1, color, muted.
type Ring = (P, f32, f32, u32, bool);

fn paint_rings(rings: Vec<Ring>, frame: (f32, f32, f32), window: &mut Window) {
    let (s, ox, oy) = frame;
    let at = |p: P| point(px(ox + p.0 * s), px(oy + p.1 * s));
    for (c, r, level, color, muted) in rings {
        let arc = |from: f32, to: f32, width: f32, color: Hsla, window: &mut Window| {
            let steps = ((to - from).abs() * 24.).ceil().max(2.) as usize;
            let mut path = PathBuilder::stroke(px(width * s));
            for i in 0..=steps {
                let a = from + (to - from) * i as f32 / steps as f32;
                let p = at(P(c.0 + a.cos() * r, c.1 + a.sin() * r));
                if i == 0 {
                    path.move_to(p);
                } else {
                    path.line_to(p);
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        };
        let top = -std::f32::consts::FRAC_PI_2;
        let full = 2. * std::f32::consts::PI;
        if muted {
            arc(top, top + full, 2.5, alpha(0xe58a8a, 0.9), window);
            continue;
        }
        arc(top, top + full, 2.5, alpha(0x000000, 0.35), window);
        if level > 0.001 {
            arc(top, top + full * level.min(1.), 3., alpha(color, 0.95), window);
        }
    }
}

fn paint_cables(cables: Vec<Cable>, frame: (f32, f32, f32), phase: f32, window: &mut Window) {
    let (s, ox, oy) = frame;
    let at = |p: P| point(px(ox + p.0 * s), px(oy + p.1 * s));
    for (a, b, color, opacity, flow) in cables {
        let m = (a.0 + b.0) / 2.;
        let mut path = PathBuilder::stroke(px(2. * s));
        path.move_to(at(a));
        path.cubic_bezier_to(at(b), at(P(m, a.1)), at(P(m, b.1)));
        if let Ok(path) = path.build() {
            window.paint_path(path, alpha(color, opacity * 0.35));
        }
        // Beads travel along the cable toward where the sound goes.
        let n = ((a.dist(b) / 26.).ceil() as usize).clamp(3, 60);
        for k in 0..n {
            let t = ((k as f32 + phase * flow) / n as f32).fract();
            let p = bezier(a, b, t);
            let r = 2.6 * s;
            window.paint_quad(
                fill(Bounds::centered_at(at(p), size(px(r * 2.), px(r * 2.))), alpha(color, opacity))
                    .corner_radii(px(r)),
            );
        }
    }
}

impl Render for SoundMap {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut cx.global_mut::<Core>().map_close_requested) {
            self.close(window, cx);
        }
        if self.closing.is_some_and(|at| at.elapsed() >= FADE) {
            let _ = cx.global::<Core>().worker.send(Command::Metering(false));
            window.defer(cx, |window, _| window.remove_window());
        }
        self.refresh(cx);
        let fade = self.shown_amount();
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let s = (vw / (PLANE.0 + 40.)).min(vh / (PLANE.1 + 40.)).clamp(0.5, 1.4);
        self.frame = (s, (vw - PLANE.0 * s) / 2., (vh - PLANE.1 * s) / 2.);
        let (_, ox, oy) = self.frame;
        let x = |v: f32| px(ox + v * s);
        let y = |v: f32| px(oy + v * s);
        let d = |v: f32| px(v * s);

        let zones = self.zones(cx);
        let slots = self.slots(&zones);
        let hover = self.hover(&zones);
        let colors = self.port_colors();
        let phase = self.started.elapsed().as_secs_f32();
        let orb_home = if self.hub.is_some() { ORB } else { ORB_ALONE };
        let default_output = self.view.default_output.clone();

        // Ease every puck toward its slot; dragged ones follow the pointer.
        let (dragged, everything, delta) = match &self.drag {
            Some(Drag { grab: Grab::Apps(keys), start, at, moved: true }) => {
                (keys.clone(), false, P(at.0 - start.0, at.1 - start.1))
            }
            Some(Drag { grab: Grab::Everything, start, at, moved: true }) => {
                (vec![], true, P(at.0 - start.0, at.1 - start.1))
            }
            _ => (vec![], false, P(0., 0.)),
        };
        let mut shown = BTreeMap::new();
        for (key, slot) in &slots {
            let was = self.shown.get(key).copied().unwrap_or(*slot);
            let p = if dragged.contains(key) {
                // Pulled a little toward a hovered target, like a magnet.
                let mut p = P(slot.0 + delta.0, slot.1 + delta.1);
                if let Some(zone) = hover.as_ref().and_then(|h| zones.iter().find(|z| &z.spot == h)) {
                    p = p.lerp(zone.center(), 0.18);
                }
                p
            } else {
                was.lerp(*slot, 0.2)
            };
            shown.insert(key.clone(), p);
        }
        self.shown = shown;
        // Reading order for the arrow keys: by target, then top to bottom.
        let mut order: Vec<(usize, i32, i32, String)> = slots
            .iter()
            .filter_map(|(k, p)| {
                let app = self.view.apps.iter().find(|a| &a.key == k)?;
                let spot = self.spot_of(app);
                let zone = zones.iter().position(|z| z.spot == spot).unwrap_or(usize::MAX);
                Some((zone, (p.1 / 8.) as i32, p.0 as i32, k.clone()))
            })
            .collect();
        order.sort();
        self.order = order.into_iter().map(|(_, _, _, k)| k).collect();
        if self.focused.as_ref().is_some_and(|f| !self.order.contains(f)) {
            self.focused = None;
        }
        // Every frame while something moves; otherwise 30 per second for
        // the flowing cables and levels.
        let settling = self.shown.iter().any(|(k, p)| slots.get(k).is_some_and(|s| p.dist(*s) > 0.5));
        if self.drag.is_some() || self.lasso.is_some() || self.closing.is_some() || fade < 1. || settling {
            window.request_animation_frame();
        } else if self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                let _ = this.update(cx, |this, cx| {
                    this.tick = None;
                    cx.notify();
                });
            }));
        }
        let orb = if everything {
            let p = P(orb_home.0 + delta.0, orb_home.1 + delta.1);
            hover
                .as_ref()
                .and_then(|h| zones.iter().find(|z| &z.spot == h))
                .map_or(p, |z| p.lerp(z.center(), 0.18))
        } else {
            orb_home
        };

        let spot_of: BTreeMap<String, Spot> =
            self.view.apps.iter().map(|a| (a.key.clone(), self.spot_of(a))).collect();
        let on_studio = spot_of.values().filter(|s| matches!(s, Spot::Port(_))).count();
        let dark = self.hub.is_some() && on_studio == 0;
        let count = |spot: &Spot| spot_of.values().filter(|s| *s == spot).count();
        let zone_color = |spot: &Spot| zones.iter().find(|z| &z.spot == spot).map_or(LOOSE_COLOR, |z| z.color);

        // Cables (orbit view only).
        let mut cables: Vec<Cable> = Vec::new();
        if !self.minimal {
            let hub_ports: Vec<String> =
                self.hub.as_ref().map_or_else(Vec::new, |h| h.ports.iter().map(|(n, _)| n.clone()).collect());
            for (c, port) in port_positions(hub_ports.len()).iter().enumerate() {
                let live = count(&Spot::Port(c as u8 + 1)) > 0;
                let level = self.out_levels.get(&hub_ports[c]).copied().unwrap_or(0.);
                cables.push((*port, HUB, colors[c], if live { 0.35 + level * 0.65 } else { 0.08 }, if live { 0.6 + level * 1.6 } else { 0. }));
            }
            for app in &self.view.apps {
                let Some(p) = self.shown.get(&app.key).copied() else { continue };
                if everything {
                    cables.push((p, orb, TEXT, 0.7, 2.));
                    continue;
                }
                let spot = if dragged.contains(&app.key) {
                    match &hover {
                        Some(h) => h.clone(),
                        None => continue,
                    }
                } else {
                    spot_of[&app.key].clone()
                };
                if spot == Spot::Loose {
                    continue;
                }
                let Some(zone) = zones.iter().find(|z| z.spot == spot) else { continue };
                let level = self.app_levels.get(&app.key).copied().unwrap_or(0.);
                let flow = if app.running { 0.8 + level * 1.5 } else { 0. };
                cables.push((p, zone.center(), zone.color, if app.running { 0.85 } else { 0.4 }, flow));
            }
        }

        // Volume rings: an application below full volume, muted, or being
        // adjusted shows its level around its puck.
        let mut rings: Vec<Ring> = Vec::new();
        if !self.minimal {
            for app in &self.view.apps {
                let Some(p) = self.shown.get(&app.key).copied() else { continue };
                let volume = self.volume_of(&app.key);
                let muted = self.muted(&app.key);
                if volume > 0.995 && !muted && !self.adjusting(&app.key) {
                    continue;
                }
                let spot = &spot_of[&app.key];
                let crowd = count(spot);
                let size = zones.iter().find(|z| &z.spot == spot).map_or(puck_size(crowd), |z| zone_puck(z, crowd));
                rings.push((p, size / 2. + 5., volume, zone_color(spot), muted));
            }
        }
        let frame = self.frame;
        let mut root = div()
            .id("sound-map")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(hsla(0., 0., 0.02, DIM * fade))
            .opacity(fade)
            .text_color(rgb(TEXT))
            .font_family("Segoe UI")
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(
                canvas(|_, _, _| (), move |_, _, window, _| {
                    paint_cables(cables, frame, phase, window);
                    paint_rings(rings, frame, window);
                })
                    .absolute()
                    .size_full(),
            );

        // Header.
        let summary = format!(
            "{} playing · {} placed · {} outputs",
            self.view.apps.iter().filter(|a| a.running).count(),
            self.view.apps.iter().filter(|a| a.pinned.is_some()).count(),
            self.view.outputs.len()
        );
        root = root.child(
            div()
                .absolute()
                .left(x(32.))
                .top(y(20.))
                .w(d(1216.))
                .flex()
                .items_start()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(d(4.))
                        .child(div().text_size(d(26.)).font_weight(FontWeight::BOLD).child("Wavelane"))
                        .child(div().text_size(d(13.)).text_color(rgb(MUTED)).child(
                            "Drag an app onto an output, or pick one with the arrow keys and press an output's key (Shift moves everything). Scroll an app for its volume, middle-click to mute. Ctrl+Z undoes, Esc closes.",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(d(10.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_size(d(11.)).text_color(rgb(MUTED)).child(summary))
                        .child(
                            div()
                                .id("map-devices")
                                .px(d(12.))
                                .py(d(7.))
                                .rounded(d(8.))
                                .border_1()
                                .border_color(rgb(if self.edit { 0xa6bfe7 } else { 0x404247 }))
                                .bg(alpha(0x2d3035, 0.85))
                                .text_size(d(12.))
                                .cursor_pointer()
                                .hover(|b| b.bg(rgb(0x363a40)))
                                .child(if self.edit { "Done" } else { "Devices" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.edit = !this.edit;
                                    cx.notify();
                                }))
                                .test_support(),
                        )
                        .child(
                            div()
                                .id("map-minimal")
                                .px(d(12.))
                                .py(d(7.))
                                .rounded(d(8.))
                                .border_1()
                                .border_color(rgb(0x404247))
                                .bg(alpha(0x2d3035, 0.85))
                                .text_size(d(12.))
                                .cursor_pointer()
                                .hover(|b| b.bg(rgb(0x363a40)))
                                .child(if self.minimal { "Orbit view" } else { "Minimal view" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.minimal = !this.minimal;
                                    cx.global_mut::<Core>().map_minimal = this.minimal;
                                    cx.notify();
                                }))
                                .test_support(),
                        )
                        .child(
                            div()
                                .id("map-close")
                                .size(d(32.))
                                .rounded(d(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .border_1()
                                .border_color(rgb(0x404247))
                                .bg(alpha(0x2d3035, 0.85))
                                .cursor_pointer()
                                .hover(|b| b.bg(rgb(0x363a40)))
                                .child(svg().path("console/close.svg").size(d(16.)).text_color(rgb(TEXT)))
                                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))
                                .test_support(),
                        ),
                ),
        );

        if self.view.phase != Phase::Running {
            root = root.child(
                div()
                    .absolute()
                    .left(x(340.))
                    .top(y(380.))
                    .w(d(600.))
                    .text_center()
                    .text_size(d(16.))
                    .text_color(rgb(MUTED))
                    .child(self.view.error.clone().unwrap_or_else(|| "Connecting to PipeWire…".into())),
            );
            return root;
        }

        if let (false, Some(hub)) = (self.minimal, self.hub.clone()) {
            // The hub: also a drop target (its first port).
            let hot = hover == Some(Spot::Studio);
            let glow = if hot {
                1.
            } else {
                hub.ports.iter().filter_map(|(n, _)| self.out_levels.get(n).copied()).fold(0., f32::max)
            };
            root = root.child(
                div()
                    .absolute()
                    .left(x(HUB.0 - 100.))
                    .top(y(HUB.1 - 55.))
                    .w(d(200.))
                    .h(d(110.))
                    .rounded(d(22.))
                    .border(d(if hot { 2. } else { 1. }))
                    .border_color(rgb(if hot {
                        colors[0]
                    } else if dark {
                        0x2d3035
                    } else {
                        0x4a4d53
                    }))
                    .bg(rgb(if dark && !hot { 0x1c1f23 } else { 0x2d3035 }))
                    .when(!dark || hot, |hub| {
                        hub.shadow(vec![BoxShadow {
                            color: alpha(colors[0], 0.15 + glow * 0.35),
                            offset: point(px(0.), px(0.)),
                            blur_radius: d(20. + glow * 40.),
                            spread_radius: d(glow * 6.),
                            inset: false,
                        }])
                    })
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(d(4.))
                    .child(
                        div()
                            .text_size(d(17.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(if dark { 0x747c82 } else { TEXT }))
                            .max_w(d(184.))
                            .truncate()
                            .child(hub.label.clone()),
                    )
                    .child(div().text_size(d(11.)).text_color(rgb(MUTED)).child(
                        if hot {
                            format!("to {}", hub.ports[0].1)
                        } else if dark {
                            "idle".to_owned()
                        } else {
                            format!("{on_studio} app{}", if on_studio == 1 { "" } else { "s" })
                        },
                    )),
            );
        }

        // Zones.
        let mut badges: Vec<(f32, f32, String)> = Vec::new();
        let devices_in_order: Vec<&Spot> =
            zones.iter().filter(|z| matches!(z.spot, Spot::Device(_))).map(|z| &z.spot).collect();
        for zone in &zones {
            let hot = hover.as_ref() == Some(&zone.spot);
            // Its key, in a small badge on its corner.
            let ports = colors.len();
            let key = match &zone.spot {
                Spot::Port(n) => Some(n.to_string()),
                Spot::Device(_) => devices_in_order
                    .iter()
                    .position(|d| *d == &zone.spot)
                    .map(|i| i + ports + 1)
                    .filter(|k| *k <= 9)
                    .map(|k| k.to_string()),
                Spot::Loose => Some("0".into()),
                Spot::Studio => None,
            };
            if let Some(key) = key {
                let (bx, by) = match zone.shape {
                    Shape::Circle(c, r) => (c.0 + r * 0.72, c.1 - r * 0.98),
                    Shape::Rect(a, b) => (b.0 - 14., a.1 - 1.),
                };
                badges.push((bx, by, key));
            }
            if zone.spot == Spot::Studio {
                // Orbit view: the hub above. Lanes: a bar over the channels.
                if let (true, Shape::Rect(a, b)) = (self.minimal, &zone.shape) {
                    root = root.child(
                        div()
                            .absolute()
                            .left(x(a.0))
                            .top(y(a.1))
                            .w(d(b.0 - a.0))
                            .h(d(b.1 - a.1))
                            .rounded(d(10.))
                            .border(d(1.5))
                            .border_color(if hot { rgb(zone.color).into() } else { alpha(0x404247, 0.8) })
                            .bg(alpha(if hot { 0x2a2d32 } else { 0x232529 }, 0.85))
                            .flex()
                            .items_center()
                            .gap(d(8.))
                            .px(d(12.))
                            .child(div().size(d(8.)).rounded_full().bg(rgb(if dark { 0x747c82 } else { zone.color })))
                            .child(div().text_size(d(13.)).font_weight(FontWeight::SEMIBOLD).child(zone.label.clone()))
                            .child(div().text_size(d(11.)).text_color(rgb(MUTED)).child(if dark { "idle" } else { "" })),
                    );
                }
                continue;
            }
            let n = count(&zone.spot);
            let is_default = self.output_of(&zone.spot).is_some() && self.output_of(&zone.spot) == default_output;
            let sub = match (&zone.spot, n) {
                (Spot::Loose, _) if !self.minimal => "drop to follow".to_owned(),
                (_, 0) => "empty".to_owned(),
                (_, 1) => "1 app".to_owned(),
                (_, n) => format!("{n} apps"),
            };
            let sub = if is_default { format!("default · {sub}") } else { sub };
            match zone.shape {
                Shape::Circle(c, r) => {
                    let level = self.output_of(&zone.spot).and_then(|o| self.out_levels.get(&o).copied()).unwrap_or(0.);
                    let is_port = matches!(zone.spot, Spot::Port(_));
                    let size = if hot { r * 2.2 } else { r * 2. };
                    let mut el = div()
                        .absolute()
                        .left(x(c.0 - size / 2.))
                        .top(y(c.1 - size / 2.))
                        .size(d(size))
                        .rounded_full()
                        .border(d(3.))
                        .border_color(rgb(zone.color))
                        .bg(rgb(0x232529))
                        .flex()
                        .items_center()
                        .justify_center()
                        .shadow(vec![BoxShadow {
                            color: alpha(zone.color, if hot { 0.55 } else { 0.1 + level * 0.5 }),
                            offset: point(px(0.), px(0.)),
                            blur_radius: d(if hot { 40. } else { 8. + level * 30. }),
                            spread_radius: d(if hot { 8. } else { level * 6. }),
                            inset: false,
                        }]);
                    if (is_port && dark) || zone.hidden {
                        el = el.opacity(if zone.hidden { 0.4 } else { 0.55 });
                    }
                    el = if is_port {
                        el.child(
                            div()
                                .text_size(d(26.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(zone.color))
                                .child(match zone.spot {
                                    Spot::Port(p) => p.to_string(),
                                    _ => String::new(),
                                }),
                        )
                    } else {
                        el.child(svg().path(zone.icon).size(d(r * 0.75)).text_color(rgb(zone.color)))
                    };
                    root = root.child(el);
                    // Channels: the label on the hub side, clear of the
                    // apps orbiting outward.
                    let label_y = match (is_port, c.1 < HUB.1) {
                        (true, true) => c.1 + r + 8.,
                        (true, false) => c.1 - r - 42.,
                        _ => c.1 + r + 10.,
                    };
                    let label_w = (r * 4. + 40.).min(200.);
                    root = root.child(
                        zone_label(&zone.label, &sub, x(c.0 - label_w / 2.), y(label_y), d(label_w), s)
                            .when(zone.hidden, |l| l.opacity(0.5)),
                    );
                    if self.edit && !is_port {
                        root = root.child(self.visibility_pill(zone, x(c.0 - 40.), y(label_y + 36.), d(80.), s, cx));
                    }
                }
                Shape::Rect(a, b) => {
                    let el = div()
                        .absolute()
                        .left(x(a.0))
                        .top(y(a.1))
                        .w(d(b.0 - a.0))
                        .h(d(b.1 - a.1))
                        .rounded(d(if self.minimal { 16. } else { 20. }))
                        .border(d(1.5))
                        .when(!self.minimal, |e| e.border_dashed())
                        .border_color(if hot { rgb(zone.color).into() } else { alpha(0x404247, 1.) })
                        .bg(if self.minimal {
                            alpha(if hot { 0x2a2d32 } else { 0x232529 }, 0.92)
                        } else {
                            alpha(LOOSE_COLOR, if hot { 0.14 } else { 0.03 })
                        });
                    root = root.child(el.when(zone.hidden, |e| e.opacity(0.45)));
                    if self.minimal {
                        let placed = slots.keys().filter(|k| spot_of.get(*k) == Some(&zone.spot)).count();
                        if n > placed {
                            root = root.child(
                                div()
                                    .absolute()
                                    .left(x(a.0))
                                    .top(y(b.1 - 30.))
                                    .w(d(b.0 - a.0))
                                    .text_center()
                                    .text_size(d(11.))
                                    .text_color(rgb(MUTED))
                                    .child(format!("+{} more", n - placed)),
                            );
                        }
                        if self.edit && matches!(zone.spot, Spot::Device(_)) {
                            let w = (b.0 - a.0 - 28.).min(80.);
                            root = root.child(self.visibility_pill(zone, x(a.0 + 14.), y(a.1 + 36.), d(w), s, cx));
                        }
                        let dim = (dark && matches!(zone.spot, Spot::Port(_))) || zone.hidden;
                        root = root.child(
                            div()
                                .absolute()
                                .left(x(a.0 + 14.))
                                .top(y(a.1 + 12.))
                                .w(d(b.0 - a.0 - 28.))
                                .flex()
                                .items_center()
                                .gap(d(8.))
                                .when(dim, |e| e.opacity(0.5))
                                .child(div().size(d(10.)).rounded(d(3.)).bg(rgb(zone.color)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(d(13.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(zone.label.clone()),
                                )
                                .when(n > 0, |e| e.child(div().text_size(d(11.)).text_color(rgb(MUTED)).child(n.to_string()))),
                        );
                    } else {
                        root = root.child(zone_label(&zone.label, &sub, x(a.0 - 30.), y(b.1 - 46.), d(b.0 - a.0 + 60.), s));
                    }
                }
            }
        }

        for (bx, by, key) in badges {
            root = root.child(
                div()
                    .absolute()
                    .left(x(bx - 9.))
                    .top(y(by - 9.))
                    .size(d(18.))
                    .rounded(d(5.))
                    .border_1()
                    .border_color(rgb(0x404247))
                    .bg(alpha(0x1c1f23, 0.9))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(d(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child(key),
            );
        }

        // Applications.
        for app in &self.view.apps {
            let Some(p) = self.shown.get(&app.key).copied() else { continue };
            let spot = &spot_of[&app.key];
            let lifted = dragged.contains(&app.key) || everything;
            let color = if everything {
                TEXT
            } else if let (true, Some(h)) = (lifted, &hover) {
                zone_color(h)
            } else {
                zone_color(spot)
            };
            let selected = self.selected.contains(&app.key);
            let key = app.key.clone();
            let initial = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
            let icon = crate::app_icons::find(&app.icon, &app.binary, &app.name);
            let crowd = count(spot);
            let focused = self.focused.as_ref() == Some(&app.key);
            let volume = self.volume_of(&app.key);
            let muted = self.muted(&app.key);
            let adjusting = self.adjusting(&app.key);
            let pulse = if muted { 0. } else { self.app_levels.get(&app.key).copied().unwrap_or(0.) };
            let level_text = if muted { "muted".to_owned() } else { format!("{:.0}%", volume * 100.) };
            let el = if self.minimal {
                let chip = zones
                    .iter()
                    .find(|z| &z.spot == spot)
                    .and_then(|z| match z.shape {
                        Shape::Rect(a, b) if z.spot == Spot::Loose => {
                            let per = (((b.0 - a.0) - 32.) / 162.).max(1.) as usize;
                            Some(((b.1 - a.1 - 48.) / crowd.div_ceil(per) as f32).clamp(28., 44.) - 4.)
                        }
                        Shape::Rect(a, b) => {
                            let top = if self.edit && matches!(z.spot, Spot::Device(_)) { 28. } else { 0. };
                            Some(lane_step(b.1 - a.1 - top, crowd).0 - 6.)
                        }
                        _ => None,
                    })
                    .unwrap_or(40.)
                    .min(40.);
                // Narrow lanes (many devices) take narrower chips.
                let chip_w = zones
                    .iter()
                    .find(|z| &z.spot == spot)
                    .and_then(|z| match z.shape {
                        Shape::Rect(a, b) if z.spot != Spot::Loose => Some(b.0 - a.0 - 12.),
                        _ => None,
                    })
                    .unwrap_or(150.)
                    .min(150.);
                div()
                    .id(SharedString::from(format!("map-app-{}", key_id(&app.key))))
                    .absolute()
                    .left(x(p.0 - chip_w / 2.))
                    .top(y(p.1 - chip / 2.))
                    .w(d(chip_w))
                    .h(d(chip))
                    .rounded(d(12.))
                    .border(d(1.5))
                    .border_color(if lifted || selected { rgb(color) } else { rgb(0x404247) })
                    .bg(rgb(0x2d3035))
                    .flex()
                    .items_center()
                    .gap(d(10.))
                    .pl(d(6.))
                    .pr(d(12.))
                    .when(!app.running, |e| e.opacity(0.6))
                    .when(focused, |e| e.border_color(rgb(TEXT)))
                    .child({
                        let face = d((chip - 12.).min(28.));
                        let tile = div()
                            .size(face)
                            .flex_shrink_0()
                            .rounded(d(8.))
                            .flex()
                            .items_center()
                            .justify_center();
                        match &icon {
                            Some(path) => tile
                                .bg(alpha(color, 0.22))
                                .child(img(path.clone()).size(d((chip - 16.).min(24.)))),
                            None => tile
                                .bg(rgb(color))
                                .text_size(d(13.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(0x1c1f23))
                                .child(initial),
                        }
                    })
                    .child(div().flex_1().min_w_0().truncate().text_size(d(13.)).child(app.name.clone()))
                    .when(adjusting || muted || volume < 0.995, |e| {
                        e.child(
                            div()
                                .flex_shrink_0()
                                .text_size(d(11.))
                                .text_color(rgb(if muted { 0xe58a8a } else { MUTED }))
                                .child(level_text.clone()),
                        )
                    })
            } else {
                let zone_of = zones.iter().find(|z| &z.spot == spot);
                let grid = zone_of.is_some_and(is_grid);
                let size = zone_of.map_or(puck_size(crowd), |z| zone_puck(z, crowd)) + if lifted { 6. } else { 0. };
                // The label is not part of the puck: crowded orbits put
                // labels over neighbours, and only the circle may grab.
                // Crowded, a name shows on hover (or while moving it).
                let named = (crowd <= 4 && !grid) || lifted || selected || focused || adjusting
                    || self.hovered.as_ref() == Some(&app.key);
                let label = if adjusting { format!("{} · {level_text}", app.name) } else { app.name.clone() };
                root = root.when(named, |root| root.child(
                    div()
                        .absolute()
                        .left(x(p.0 - 55.))
                        .top(y(p.1 + size / 2. + 5.))
                        .w(d(110.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .max_w(d(110.))
                                .truncate()
                                .px(d(7.))
                                .py(d(1.))
                                .rounded(d(6.))
                                .bg(alpha(0x1c1f23, 0.85))
                                .text_size(d(12.))
                                .text_color(rgb(0xc9cdd1))
                                .child(label),
                        ),
                ));
                let hover_key = app.key.clone();
                div()
                    .id(SharedString::from(format!("map-app-{}", key_id(&app.key))))
                    .absolute()
                    .left(x(p.0 - size / 2.))
                    .top(y(p.1 - size / 2.))
                    .size(d(size))
                    .rounded_full()
                    .border(d(2.5))
                    .border_color(rgb(color))
                    .bg(rgb(0x2d3035))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(d(size * 0.38))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(!app.running, |e| e.opacity(0.6))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hovered = Some(hover_key.clone());
                        } else if this.hovered.as_ref() == Some(&hover_key) {
                            this.hovered = None;
                        }
                        cx.notify();
                    }))
                    .when(muted, |e| e.opacity(0.45))
                    // Following the default output: a dashed outline.
                    .when(app.pinned.is_none(), |e| e.border_dashed())
                    .shadow(vec![BoxShadow {
                        color: if focused {
                            rgb(TEXT).into()
                        } else if selected {
                            rgb(0xa6bfe7).into()
                        } else if pulse > 0.02 {
                            // Its own sound: a glow that breathes with it.
                            alpha(color, (0.25 + pulse * 0.75).min(0.95))
                        } else {
                            alpha(0x000000, 0.45)
                        },
                        offset: point(px(0.), d(if selected || focused || pulse > 0.02 { 0. } else { 5. })),
                        blur_radius: d(if selected || focused { 0. } else if pulse > 0.02 { 6. + pulse * 26. } else { 14. }),
                        spread_radius: d(if selected || focused { 4. } else { pulse * 4. }),
                        inset: false,
                    }])
                    .child(match &icon {
                        Some(path) => img(path.clone()).size(d(size * 0.6)).into_any_element(),
                        None => div().child(initial).into_any_element(),
                    })
            };
            let (scroll_key, mute_key) = (key.clone(), key.clone());
            root = root.child(
                el.cursor_grab()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.grab_app(key.clone(), event, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| this.toggle_mute(mute_key.clone(), cx)),
                    )
                    .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                        this.scroll_volume(scroll_key.clone(), event, cx)
                    }))
                    .test_support(),
            );
        }

        // Everything.
        let (orb_size, label) = if self.minimal { (0., "") } else { (64., "Everything") };
        root = if self.minimal {
            let at = if everything { P(1150. + delta.0, 112. + delta.1) } else { P(1150., 112.) };
            root.child(
                div()
                    .id("map-everything")
                    .absolute()
                    .left(x(at.0 - 70.))
                    .top(y(at.1 - 20.))
                    .w(d(140.))
                    .h(d(40.))
                    .rounded(d(12.))
                    .border(d(1.5))
                    .border_dashed()
                    .border_color(rgb(TEXT))
                    .bg(rgb(0x2d3035))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(d(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_grab()
                    .child("All apps")
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| this.grab_everything(e, cx)))
                    .test_support(),
            )
        } else {
            root.child(
                div()
                    .id("map-everything")
                    .absolute()
                    .left(x(orb.0 - orb_size / 2.))
                    .top(y(orb.1 - orb_size / 2.))
                    .size(d(orb_size))
                    .rounded_full()
                    .border(d(2.))
                    .border_dashed()
                    .border_color(rgb(TEXT))
                    .bg(rgb(0x363a40))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_grab()
                    .shadow(vec![BoxShadow {
                        color: alpha(0xc1a9d8, if everything { 0.7 } else { 0.25 + (phase * 2.).sin().abs() * 0.15 }),
                        offset: point(px(0.), px(0.)),
                        blur_radius: d(if everything { 60. } else { 30. }),
                        spread_radius: d(2.),
                        inset: false,
                    }])
                    .child(svg().path("console/orbit.svg").size(d(28.)).text_color(rgb(TEXT)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| this.grab_everything(e, cx)))
                    .test_support(),
            )
            .child(
                div()
                    .absolute()
                    .left(x(orb_home.0 - 80.))
                    .top(y(orb_home.1 + 38.))
                    .w(d(160.))
                    .text_center()
                    .text_size(d(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child(label),
            )
        };

        if let Some((a, b)) = self.lasso.filter(|(a, b)| a.dist(*b) > 4.) {
            root = root.child(
                div()
                    .absolute()
                    .left(x(a.0.min(b.0)))
                    .top(y(a.1.min(b.1)))
                    .w(d((a.0 - b.0).abs()))
                    .h(d((a.1 - b.1).abs()))
                    .rounded(d(8.))
                    .border(d(1.5))
                    .border_color(rgb(0xa6bfe7))
                    .bg(alpha(0xa6bfe7, 0.1)),
            );
        }

        if let Some((text, at)) = &self.toast {
            let undo = self.toast_undo && self.undo.is_some();
            if at.elapsed() < Duration::from_millis(if undo { 5000 } else { 2600 }) {
                root = root.child(
                    div()
                        .absolute()
                        .left(x(390.))
                        .top(y(90.))
                        .w(d(500.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px(d(16.))
                                .py(d(8.))
                                .rounded_full()
                                .bg(rgb(TEXT))
                                .text_color(rgb(0x1c1f23))
                                .text_size(d(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .flex()
                                .items_center()
                                .gap(d(12.))
                                .child(text.clone())
                                .when(undo, |pill| {
                                    pill.child(
                                        div()
                                            .id("map-undo")
                                            .px(d(10.))
                                            .py(d(2.))
                                            .rounded_full()
                                            .bg(rgb(0x1c1f23))
                                            .text_color(rgb(TEXT))
                                            .cursor_pointer()
                                            .child("Undo")
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                            .on_click(cx.listener(|this, _, _, cx| this.undo(cx)))
                                            .test_support(),
                                    )
                                }),
                        ),
                );
            }
        }
        if let Some(message) = &self.view.message {
            root = root.child(
                div()
                    .absolute()
                    .left(x(32.))
                    .top(y(776.))
                    .text_size(d(12.))
                    .text_color(rgb(0xe58a8a))
                    .child(message.clone()),
            );
        }
        root
    }
}

/// A device planet in a grid of several (smaller than a lone device).
fn is_grid(zone: &Zone) -> bool {
    matches!((&zone.spot, &zone.shape), (Spot::Device(_), Shape::Circle(_, r)) if *r < 54.)
}

/// Puck diameter for an application in `zone`, which holds `n`.
fn zone_puck(zone: &Zone, n: usize) -> f32 {
    if is_grid(zone) {
        puck_size(n).min(32.)
    } else {
        puck_size(n)
    }
}

/// Puck diameter for a zone holding `n` applications.
fn puck_size(n: usize) -> f32 {
    (50. - 1.5 * (n as f32 - 6.).max(0.)).clamp(34., 50.)
}

/// Chip spacing in a lane of height `h` holding `n` applications, and how
/// many fit (the last row says how many more there are).
fn lane_step(h: f32, n: usize) -> (f32, usize) {
    let room = h - 56.;
    let step = (room / n.max(1) as f32).min(50.);
    if step >= 30. {
        (step, n)
    } else {
        (30., ((room / 30.) as usize).saturating_sub(1))
    }
}

fn zone_label(label: &str, sub: &str, left: Pixels, top: Pixels, width: Pixels, s: f32) -> Div {
    div()
        .absolute()
        .left(left)
        .top(top)
        .w(width)
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .max_w(width)
                .truncate()
                .text_size(px(13. * s))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.to_owned()),
        )
        .child(div().text_size(px(11. * s)).text_color(rgb(MUTED)).child(sub.to_owned()))
}

/// Open the map over the screen, or put it away when it is open.
pub(crate) fn toggle(token: Option<String>, cx: &mut App) {
    if let Some(handle) = cx.global::<Core>().map_window {
        if cx.windows().contains(&handle) {
            // Fades out, then closes.
            cx.global_mut::<Core>().map_close_requested = true;
            let _ = handle.update(cx, |_, window, _| window.refresh());
            return;
        }
    }
    cx.global_mut::<Core>().map_close_requested = false;
    if let Some(token) = token {
        gpui_linux::set_pending_activation_token(token);
    }
    let _ = cx.global::<Core>().worker.send(Command::Metering(true));
    let minimal = cx.global::<Core>().map_minimal;
    let bounds = cx
        .primary_display()
        .map(|d| d.bounds())
        .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))));
    // No window frame: its shadow margin would offset the overlay.
    let build = move |window: &mut Window, cx: &mut App| cx.new(|cx| SoundMap::new(minimal, window, cx));
    let overlay = WindowOptions {
        kind: WindowKind::LayerShell(layer_shell::LayerShellOptions {
            namespace: "wavelane".into(),
            layer: layer_shell::Layer::Overlay,
            anchor: layer_shell::Anchor::TOP
                | layer_shell::Anchor::BOTTOM
                | layer_shell::Anchor::LEFT
                | layer_shell::Anchor::RIGHT,
            keyboard_interactivity: layer_shell::KeyboardInteractivity::Exclusive,
            ..Default::default()
        }),
        // Anchored to every edge with no size: the compositor stretches it
        // over the whole monitor it opens on.
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))))),
        // The desktop shows through, blurred, as behind Plasma's overlays.
        window_background: WindowBackgroundAppearance::Blurred,
        app_id: Some("digital.zombie.wavelane".into()),
        focus: true,
        ..Default::default()
    };
    let handle = match cx.open_window(overlay, build) {
        Ok(handle) => Some(handle),
        Err(error) => {
            // No overlay layer (GNOME): a transparent fullscreen window.
            eprintln!("Wavelane: {error}; using a fullscreen window");
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Fullscreen(bounds)),
                    window_background: WindowBackgroundAppearance::Transparent,
                    window_decorations: Some(WindowDecorations::Client),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Wavelane".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    app_id: Some("digital.zombie.wavelane".into()),
                    ..Default::default()
                },
                build,
            )
            .map_err(|e| eprintln!("Wavelane map unavailable: {e}"))
            .ok()
        }
    };
    cx.global_mut::<Core>().map_window = handle.map(|h| h.into());
    cx.activate(true);
}

/// Same-user diagnostics: inspect the open map and feed it real pointer and
/// key events, so drags are tested through the map's own input path.
#[cfg(test)]
mod tests {
    #[test]
    fn device_names_keep_the_useful_part() {
        assert_eq!(
            super::friendly("GA102 High Definition Audio Controller Digital Stereo (HDMI 2) [BenQ GL2760]"),
            "BenQ GL2760 · HDMI 2"
        );
        assert_eq!(super::friendly("Speakers"), "Speakers");
        assert_eq!(super::friendly("Family 17h HD Audio Analog Stereo"), "Family 17h HD Audio Analog Stereo");
    }
}

