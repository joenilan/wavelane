//! The tray flyout: the default output's volume, then every playing
//! application with its own volume, mute and output, and the way to the map.
//! It opens in the tray's corner on the overlay layer and closes when it
//! loses focus (a click anywhere else), on Esc, or on another tray click.
use crate::app::{key_id, Core};
use crate::engine::{Command, Phase, View};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

const WIDTH: f32 = 380.;
const ROW: f32 = 62.;
/// Rows shown before the list scrolls.
const MAX_ROWS: usize = 7;
const TEXT: u32 = 0xe5e7e8;
const MUTED: u32 = 0xa0a6ad;
const ACCENT: u32 = 0xa6bfe7;
const LINE: u32 = 0x404247;
const FADE: Duration = Duration::from_millis(160);

fn alpha(color: u32, a: f32) -> Hsla {
    let mut c: Hsla = rgb(color).into();
    c.a = a;
    c
}

/// What a slider changes.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Knob {
    Output(String),
    App(String),
}

pub struct Flyout {
    view: View,
    viewed: Instant,
    started: Instant,
    /// The app whose output choices are showing; "" for the default output.
    choosing: Option<String>,
    /// The slider being dragged.
    dragging: Option<Knob>,
    /// Each slider track's bounds from the last frame.
    tracks: Rc<RefCell<HashMap<Knob, Bounds<Pixels>>>>,
    /// Values just set, shown until PipeWire echoes them.
    set: Vec<(Knob, f32, Instant)>,
    focus: FocusHandle,
    _activation: Subscription,
    /// Redraws when the graph changes while it is open.
    _watch: Task<()>,
}

impl Flyout {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // A click anywhere else closes it, like any tray popup.
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() && this.started.elapsed() > Duration::from_millis(300) {
                window.remove_window();
            }
        });
        let watch = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if cx.global::<Core>().worker.view().differs(&this.view) {
                    cx.notify();
                }
            }) else {
                break;
            };
        });
        Self {
            view: cx.global::<Core>().worker.view(),
            viewed: Instant::now(),
            started: Instant::now(),
            choosing: None,
            dragging: None,
            tracks: Rc::default(),
            set: Vec::new(),
            focus,
            _activation: activation,
            _watch: watch,
        }
    }

    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        let _ = cx.global::<Core>().worker.send(command);
        cx.notify();
    }

    fn value(&self, knob: &Knob) -> f32 {
        if let Some((_, v, at)) = self.set.iter().rev().find(|(k, _, _)| k == knob) {
            if at.elapsed() < Duration::from_millis(1500) {
                return *v;
            }
        }
        match knob {
            Knob::Output(name) => self.view.outputs.iter().find(|o| &o.name == name).and_then(|o| o.volume),
            Knob::App(key) => self.view.apps.iter().find(|a| &a.key == key).and_then(|a| a.volume),
        }
        .unwrap_or(1.)
        .clamp(0., 1.)
    }

    fn muted(&self, knob: &Knob) -> bool {
        match knob {
            Knob::Output(name) => self.view.outputs.iter().find(|o| &o.name == name).is_some_and(|o| o.mute),
            Knob::App(key) => self.view.apps.iter().find(|a| &a.key == key).is_some_and(|a| a.mute),
        }
    }

    fn apply(&mut self, knob: Knob, volume: f32, cx: &mut Context<Self>) {
        let volume = (volume * 100.).round().clamp(0., 100.) / 100.;
        self.set.retain(|(k, _, at)| k != &knob && at.elapsed() < Duration::from_millis(1500));
        self.set.push((knob.clone(), volume, Instant::now()));
        let command = match knob {
            Knob::Output(name) => Command::OutputLevel { name, volume: Some(volume), mute: None },
            Knob::App(key) => Command::AppLevel { key, volume: Some(volume), mute: None },
        };
        self.send(command, cx);
    }

    fn toggle_mute(&mut self, knob: Knob, cx: &mut Context<Self>) {
        let mute = Some(!self.muted(&knob));
        let command = match knob {
            Knob::Output(name) => Command::OutputLevel { name, volume: None, mute },
            Knob::App(key) => Command::AppLevel { key, volume: None, mute },
        };
        self.send(command, cx);
    }

    /// Set a slider from a pointer position over its track.
    fn slide(&mut self, knob: &Knob, x: Pixels, cx: &mut Context<Self>) {
        let Some(track) = self.tracks.borrow().get(knob).copied() else { return };
        let width = f32::from(track.size.width).max(1.);
        self.apply(knob.clone(), f32::from(x - track.origin.x) / width, cx);
    }

    /// A volume slider: click or drag anywhere on it; scroll for 5% steps.
    fn slider(&self, knob: Knob, color: u32, cx: &mut Context<Self>) -> impl IntoElement {
        let value = self.value(&knob);
        let muted = self.muted(&knob);
        let id = match &knob {
            Knob::Output(n) => format!("fly-out-{}", key_id(n)),
            Knob::App(k) => format!("fly-app-{}", key_id(k)),
        };
        let (press, wheel, record) = (knob.clone(), knob.clone(), knob);
        let tracks = self.tracks.clone();
        div()
            .id(SharedString::from(id))
            .relative()
            .flex_1()
            .h(px(22.))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.dragging = Some(press.clone());
                    this.slide(&press, event.position.x, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let lines = match event.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => f32::from(p.y) / 40.,
                };
                if lines != 0. {
                    let v = this.value(&wheel) + lines * 0.05;
                    this.apply(wheel.clone(), v, cx);
                }
            }))
            .child(
                canvas(move |bounds, _, _| {
                    tracks.borrow_mut().insert(record, bounds);
                }, |_, _, _, _| {})
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(5.))
                    .rounded_full()
                    .bg(alpha(0xffffff, 0.12))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(relative(value))
                            .rounded_full()
                            .bg(if muted { alpha(MUTED, 0.45) } else { rgb(color).into() }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(-5.5))
                            .left(relative(value))
                            .ml(px(-8.))
                            .size(px(16.))
                            .rounded_full()
                            .bg(rgb(if muted { MUTED } else { TEXT }))
                            .shadow(vec![BoxShadow {
                                color: alpha(0x000000, 0.4),
                                offset: point(px(0.), px(1.)),
                                blur_radius: px(4.),
                                spread_radius: px(0.),
                                inset: false,
                            }]),
                    ),
            )
    }

    fn mute_button(&self, knob: Knob, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = self.muted(&knob);
        let id = match &knob {
            Knob::Output(n) => format!("fly-mute-out-{}", key_id(n)),
            Knob::App(k) => format!("fly-mute-{}", key_id(k)),
        };
        div()
            .id(SharedString::from(id))
            .size(px(28.))
            .flex_shrink_0()
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|b| b.bg(alpha(0xffffff, 0.08)))
            .child(
                svg()
                    .path(if muted { "console/muted.svg" } else { "console/speaker.svg" })
                    .size(px(16.))
                    .text_color(rgb(if muted { 0xe58a8a } else { MUTED })),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_mute(knob.clone(), cx)))
    }

    /// Output choices for an app ("" for the default output), as chips.
    fn choices(&self, key: &str, current: Option<&str>, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().id(SharedString::from(format!("fly-choices-{}", key_id(key)))).flex().gap(px(6.)).overflow_x_scroll();
        let hidden = &cx.global::<Core>().hidden_outputs;
        for output in self.view.outputs.iter().filter(|o| !hidden.contains(&o.name) || Some(o.name.as_str()) == current) {
            let picked = Some(output.name.as_str()) == current;
            let (app, name) = (key.to_owned(), output.name.clone());
            row = row.child(
                div()
                    .id(SharedString::from(format!("fly-pick-{}-{}", key_id(key), key_id(&output.name))))
                    .flex_shrink_0()
                    .px(px(10.))
                    .py(px(4.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if picked { ACCENT } else { LINE }))
                    .bg(alpha(if picked { ACCENT } else { 0xffffff }, if picked { 0.18 } else { 0.04 }))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(|b| b.bg(alpha(0xffffff, 0.1)))
                    .child(crate::map::friendly(&output.description))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let command = if app.is_empty() {
                            Command::PlaceAll(Some(name.clone()))
                        } else {
                            Command::Place(app.clone(), Some(name.clone()))
                        };
                        this.choosing = None;
                        this.send(command, cx);
                    })),
            );
        }
        if !key.is_empty() {
            let app = key.to_owned();
            row = row.child(
                div()
                    .id(SharedString::from(format!("fly-pick-{}-default", key_id(key))))
                    .flex_shrink_0()
                    .px(px(10.))
                    .py(px(4.))
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(LINE))
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .child("Follow default")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choosing = None;
                        this.send(Command::Place(app.clone(), None), cx);
                    })),
            );
        }
        row
    }

    /// A small chip naming where something plays; click to choose.
    fn output_chip(&self, key: &str, label: String, follows: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let key = key.to_owned();
        div()
            .id(SharedString::from(format!("fly-chip-{}", key_id(&key))))
            .max_w(px(170.))
            .truncate()
            .px(px(8.))
            .py(px(1.))
            .rounded_full()
            .border_1()
            .when(follows, |c| c.border_dashed())
            .border_color(rgb(LINE))
            .text_size(px(11.))
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .hover(|b| b.text_color(rgb(TEXT)).border_color(rgb(ACCENT)))
            .child(label)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.choosing = if this.choosing.as_deref() == Some(key.as_str()) { None } else { Some(key.clone()) };
                cx.notify();
            }))
    }
}

impl Flyout {
    /// A newer Wavelane: what is happening, and the one button for the next
    /// step (Update downloads and installs; Restart starts the new version).
    fn update_row(&self, cx: &mut Context<Self>) -> Div {
        use crate::updates::Phase;
        let view = cx.global::<Core>().updates.view();
        let latest = view.latest.clone().unwrap_or_default();
        let text = match (&view.phase, view.ineligible) {
            (Phase::Checking, _) => "Checking for updates…".to_owned(),
            (Phase::UpToDate, _) => format!("Wavelane {} is up to date.", view.current),
            (Phase::Downloading { percent }, _) => format!("Downloading Wavelane {latest}… {percent}%"),
            (Phase::Ready | Phase::Installing, _) => format!("Installing Wavelane {latest}…"),
            (Phase::Installed, _) => format!("Wavelane {latest} is installed."),
            (Phase::Failed, _) => view.error.clone().unwrap_or_else(|| "The update failed.".into()),
            (_, Some(reason)) => format!("Wavelane {latest} is available. {reason}"),
            _ => format!("Wavelane {latest} is available (you have {}).", view.current),
        };
        let button = |id: &'static str, label: &str| {
            div()
                .id(id)
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .rounded(px(8.))
                .bg(alpha(ACCENT, 0.2))
                .border_1()
                .border_color(alpha(ACCENT, 0.6))
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|b| b.bg(alpha(ACCENT, 0.3)))
                .child(label.to_owned())
        };
        let action = match view.phase {
            Phase::Available | Phase::Failed if view.ineligible.is_none() && view.latest.is_some() => Some(
                button("fly-update", "Update").on_click(cx.listener(|_, _, _, cx| {
                    let core = cx.global_mut::<Core>();
                    core.install_when_ready = true;
                    core.updates.start_download();
                    cx.notify();
                })),
            ),
            Phase::Installed => Some(button("fly-restart", "Restart").on_click(cx.listener(|_, _, _, cx| {
                // Back in the tray, as it was.
                match cx.global::<Core>().updates.relaunch(&["--hidden".to_owned()], None) {
                    Ok(()) => {
                        cx.global_mut::<Core>().worker.stop();
                        cx.quit();
                    }
                    Err(error) => eprintln!("Wavelane: {error}"),
                }
            }))),
            Phase::Available if view.ineligible.is_some() => Some(
                button("fly-release", "Releases").on_click(|_, _, cx| cx.open_url(crate::updates::RELEASE_PAGE)),
            ),
            _ => None,
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(alpha(0xffffff, 0.05))
            .child(div().flex_1().text_size(px(12.)).text_color(rgb(TEXT)).child(text))
            .children(action)
    }
}

impl Render for Flyout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.viewed.elapsed() >= Duration::from_millis(150) {
            self.view = cx.global::<Core>().worker.view();
            self.viewed = Instant::now();
        }
        // Apps come and go while it is open: it grows or shrinks with them.
        let h = height(&self.view, shows_update(cx));
        if (f32::from(window.viewport_size().height) - h).abs() > 0.5 {
            window.resize(size(px(WIDTH), px(h)));
        }
        let fade = (self.started.elapsed().as_secs_f32() / FADE.as_secs_f32()).min(1.);
        // Redraw while fading in or dragging a slider.
        if fade < 1. || self.dragging.is_some() {
            window.request_animation_frame();
        }
        let default = self.view.default_output.clone();
        let output = default.as_ref().and_then(|d| self.view.outputs.iter().find(|o| &o.name == d)).cloned();
        let mut apps = self.view.apps.clone();
        apps.sort_by_key(|a| (!a.running, a.name.to_lowercase()));
        let name_of = |name: &str| {
            self.view
                .outputs
                .iter()
                .find(|o| o.name == name)
                .map_or_else(|| name.to_owned(), |o| crate::map::friendly(&o.description))
        };

        let mut panel = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(14.))
            .rounded(px(14.))
            .border_1()
            .border_color(alpha(LINE, 0.9))
            .bg(alpha(0x1f2125, 0.98))
            .text_color(rgb(TEXT))
            .font_family("Segoe UI")
            .shadow(vec![BoxShadow {
                color: alpha(0x000000, 0.45),
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            }]);

        // Header.
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Wavelane"))
                .child(div().flex_1())
                .child(
                    div()
                        .id("fly-map")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(LINE))
                        .text_size(px(12.))
                        .cursor_pointer()
                        .hover(|b| b.bg(alpha(0xffffff, 0.08)))
                        .child(svg().path("console/orbit.svg").size(px(14.)).text_color(rgb(TEXT)))
                        .child("Open map")
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.remove_window();
                            crate::map::toggle(None, cx);
                        })),
                ),
        );

        if shows_update(cx) {
            panel = panel.child(self.update_row(cx));
        }

        if self.view.phase != Phase::Running {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .justify_end()
                .child(panel.child(div().text_size(px(13.)).text_color(rgb(MUTED)).child(
                    self.view.error.clone().unwrap_or_else(|| "Connecting to PipeWire…".into()),
                )))
                .into_any_element();
        }

        // The default output: its volume, and where it is.
        if let Some(output) = &output {
            let knob = Knob::Output(output.name.clone());
            let percent = format!("{:.0}%", self.value(&knob) * 100.);
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child("Sound output"))
                            .child(self.output_chip("", name_of(&output.name), false, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(self.mute_button(knob.clone(), cx))
                            .child(self.slider(knob, ACCENT, cx))
                            .child(div().w(px(36.)).text_size(px(12.)).text_color(rgb(MUTED)).child(percent)),
                    )
                    .when(self.choosing.as_deref() == Some(""), |e| e.child(self.choices("", default.as_deref(), cx))),
            );
        }
        panel = panel.child(div().h(px(1.)).bg(alpha(LINE, 0.8)));

        // Applications.
        let mut list = div().id("fly-apps").flex().flex_col().gap(px(4.)).overflow_y_scroll().max_h(px(ROW * MAX_ROWS as f32));
        if apps.is_empty() {
            list = list.child(div().py(px(10.)).text_size(px(13.)).text_color(rgb(MUTED)).child("Nothing is playing."));
        }
        for app in &apps {
            let knob = Knob::App(app.key.clone());
            let icon = crate::app_icons::find(&app.icon, &app.binary, &app.name);
            let initial = app.name.chars().next().unwrap_or('?').to_uppercase().to_string();
            let on = app.playing_on.clone().or_else(|| app.pinned.clone()).or_else(|| default.clone());
            let follows = app.pinned.is_none();
            let label = on.as_deref().map_or_else(|| "nowhere".to_owned(), &name_of);
            let percent = format!("{:.0}%", self.value(&knob) * 100.);
            let choosing = self.choosing.as_deref() == Some(app.key.as_str());
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(4.))
                    .when(!app.running, |e| e.opacity(0.55))
                    .child(
                        div()
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(alpha(0xffffff, 0.05))
                            .child(match icon {
                                Some(path) => img(path).size(px(26.)).into_any_element(),
                                None => div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(initial).into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().min_w_0().truncate().text_size(px(13.)).child(app.name.clone()))
                                    .child(self.output_chip(&app.key, label, follows, cx)),
                            )
                            .child(if choosing {
                                self.choices(&app.key, on.as_deref(), cx).into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(self.slider(knob.clone(), ACCENT, cx))
                                    .child(div().w(px(34.)).text_size(px(11.)).text_color(rgb(MUTED)).child(percent))
                                    .into_any_element()
                            }),
                    )
                    .child(self.mute_button(knob, cx)),
            );
        }
        panel = panel.child(list);
        // The key the desktop actually bound (changeable in its settings).
        if let Some(key) = cx.global::<Core>().hotkey.trigger() {
            panel = panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0x747c82))
                    .child(format!("{key} opens the map anytime.")),
            );
        }

        div()
            .id("flyout")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .opacity(fade)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(knob) = this.dragging.clone() {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.slide(&knob, event.position.x, cx);
                    } else {
                        this.dragging = None;
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.dragging = None;
                cx.notify();
            }))
            .child(panel)
            .into_any_element()
    }
}

/// Height for the flyout's contents now (the window keeps it while open).
fn height(view: &View, update: bool) -> f32 {
    let rows = view.apps.len().clamp(1, MAX_ROWS) as f32;
    // Header, output block, divider, list, footer, paddings (and the update).
    40. + 74. + 13. + rows * ROW + 28. + 28. + 40. + if update { UPDATE_ROW } else { 0. }
}

/// The update row's height with its gap.
const UPDATE_ROW: f32 = 64.;

/// Whether the flyout shows the update row: a newer version, its download
/// or install, or the answer to "Check for updates".
fn shows_update(cx: &App) -> bool {
    use crate::updates::Phase;
    let core = cx.global::<Core>();
    let view = core.updates.view();
    match view.phase {
        Phase::Idle => false,
        Phase::Checking | Phase::UpToDate => core.update_asked,
        Phase::Failed => core.update_asked || view.latest.is_some(),
        _ => view.latest.is_some(),
    }
}

/// Open the flyout in the tray's corner, or close it when open.
pub(crate) fn toggle(cx: &mut App) {
    if let Some(handle) = cx.global::<Core>().flyout_window {
        if cx.windows().contains(&handle) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            cx.global_mut::<Core>().flyout_window = None;
            return;
        }
    }
    let h = height(&cx.global::<Core>().worker.view(), shows_update(cx));
    let options = WindowOptions {
        kind: WindowKind::LayerShell(layer_shell::LayerShellOptions {
            namespace: "wavelane-flyout".into(),
            layer: layer_shell::Layer::Overlay,
            // The tray's corner; the compositor keeps it clear of panels.
            anchor: layer_shell::Anchor::BOTTOM | layer_shell::Anchor::RIGHT,
            margin: Some((px(0.), px(10.), px(10.), px(0.))),
            keyboard_interactivity: layer_shell::KeyboardInteractivity::OnDemand,
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(WIDTH), px(h))))),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("digital.zombie.wavelane".into()),
        focus: true,
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| cx.new(|cx| Flyout::new(window, cx))) {
        Ok(handle) => cx.global_mut::<Core>().flyout_window = Some(handle.into()),
        Err(error) => eprintln!("Wavelane flyout unavailable: {error}"),
    }
}
