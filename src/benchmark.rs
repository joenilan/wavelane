//! Opt-in, synthetic UI measurements. No PipeWire, shortcuts, or update checks.
use crate::{
    app::Core,
    engine::{App as AudioApp, Command, Phase, View},
    graph::Output,
};
use gpui_kit::*;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub static METERING: AtomicBool = AtomicBool::new(false);
pub static RENDERS: AtomicU64 = AtomicU64::new(0);
pub static COMMANDS: AtomicU64 = AtomicU64::new(0);
pub fn requested() -> bool {
    static REQUESTED: OnceLock<bool> = OnceLock::new();
    *REQUESTED.get_or_init(|| std::env::args().any(|a| a == "--benchmark"))
}
fn scene() -> String {
    std::env::var("WAVELANE_BENCH_SCENE").unwrap_or_else(|_| "tray".into())
}
pub fn fixture() -> View {
    let outputs: Vec<_> = (0..6)
        .map(|i| Output {
            node: 100 + i,
            serial: 100 + i as u64,
            name: format!("bench-output-{i}"),
            description: format!("Test output {}", i + 1),
            device: if i < 4 { 1 } else { 2 + i as u64 },
            device_description: "Test mixer".into(),
            volume: Some(0.75),
            ..Default::default()
        })
        .collect();
    View {
        phase: Phase::Running,
        default_output: Some(outputs[0].name.clone()),
        apps: (0..16)
            .map(|i| AudioApp {
                key: format!("Test app {i:02}"),
                name: format!("Test app {i:02}"),
                nodes: vec![200 + i],
                streams: 1,
                running: scene() != "map-idle",
                volume: Some(0.75),
                playing_on: Some(outputs[i as usize % 6].name.clone()),
                pinned: Some(outputs[i as usize % 6].name.clone()),
                ..Default::default()
            })
            .collect(),
        outputs,
        ..Default::default()
    }
}
pub fn apply(view: &mut View, command: Command) {
    match command {
        Command::Metering(on) => {
            METERING.store(on, Ordering::Relaxed);
        }
        Command::Place(key, output) => {
            if let Some(app) = view.apps.iter_mut().find(|a| a.key == key) {
                app.pinned = output.clone();
                app.playing_on = output.or_else(|| view.default_output.clone());
            }
            COMMANDS.fetch_add(1, Ordering::Relaxed);
        }
        Command::PlaceAll(output) => {
            if output.is_some() {
                view.default_output = output;
            }
            for app in &mut view.apps {
                app.pinned = None;
                app.playing_on = view.default_output.clone();
            }
            COMMANDS.fetch_add(1, Ordering::Relaxed);
        }
        Command::AppLevel { key, volume, mute } => {
            if let Some(app) = view.apps.iter_mut().find(|a| a.key == key) {
                if volume.is_some() {
                    app.volume = volume;
                }
                if let Some(mute) = mute {
                    app.mute = mute;
                }
            }
            COMMANDS.fetch_add(1, Ordering::Relaxed);
        }
        Command::OutputLevel { name, volume, mute } => {
            if let Some(output) = view.outputs.iter_mut().find(|o| o.name == name) {
                if volume.is_some() {
                    output.volume = volume;
                }
                if let Some(mute) = mute {
                    output.mute = mute;
                }
            }
            COMMANDS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn start(cx: &mut App) {
    // Refuse accidental launch into the owner's session. The runner creates these.
    assert_eq!(
        std::env::var("WAVELANE_BENCHMARK_PRIVATE").as_deref(),
        Ok("1")
    );
    for name in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"] {
        assert!(
            std::env::var(name).is_ok_and(|v| v.contains("gpui-evaluation")),
            "isolated {name} required"
        );
    }
    let scene = scene();
    assert!(
        [
            "tray",
            "flyout",
            "map-idle",
            "map-music",
            "interaction",
            "close-map"
        ]
        .contains(&scene.as_str())
    );
    if scene == "flyout" {
        crate::flyout::toggle(cx);
    } else if scene != "tray" {
        crate::map::toggle(None, cx);
    }
    cx.spawn(async move |cx| {
        let started = Instant::now();
        let seconds = std::env::var("WAVELANE_BENCH_SECONDS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(20);
        let mut before = None;
        let mut cycle = 0;
        let mut drag_origin = point(px(0.), px(0.));
        let mut forced_closed = false;
        let mut ready = false;
        let mut renders_before = 0;
        loop {
            cx.background_executor().timer(Duration::from_millis(33)).await;
            let elapsed = started.elapsed().as_secs_f64();
            cx.update(|cx| {
                let core = cx.global::<Core>();
                let handle = core.map_window.or(core.flyout_window);
                if scene == "close-map" && elapsed > 2. && !forced_closed {
                    if let Some(handle) = handle { let _ = handle.update(cx, |_, window, _| window.remove_window()); }
                    forced_closed = true;
                }
                if elapsed >= 5. && !ready {
                    if let Some(handle) = handle {
                        let _ = handle.update(cx, |_, window, _| { before = Some(window.frame_duration_snapshot()); });
                    }
                    renders_before = RENDERS.load(Ordering::Relaxed);
                    ready = true;
                    eprintln!("BENCH_READY scene={scene} pid={}", std::process::id());
                }
                if scene == "interaction" && elapsed > 5. {
                    if let Some(handle) = handle.and_then(|h| h.downcast::<crate::map::SoundMap>()) {
                        let positions = handle.update(cx, |map, _, _| (map.benchmark_position(), map.benchmark_target()));
                        if let Ok((base, target)) = positions {
                        let _ = AnyWindowHandle::from(handle).update(cx, |_, window, cx| {
                            let step = cycle % 120;
                            if step == 0 {
                                drag_origin = base;
                                window.dispatch_event(MouseDownEvent { button: MouseButton::Left, position: base, click_count: 1, ..Default::default() }.to_platform_input(), cx);
                            } else if step < 60 {
                                let a = step as f32 / 60. * std::f32::consts::PI;
                                let position = drag_origin + point(px(a.sin()*120.), px(-a.sin()*70.));
                                window.dispatch_event(MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), ..Default::default() }.to_platform_input(), cx);
                            } else if step == 60 {
                                window.dispatch_event(MouseUpEvent { button: MouseButton::Left, position: target, click_count: 1, ..Default::default() }.to_platform_input(), cx);
                            } else if step % 4 == 0 {
                                window.dispatch_event(ScrollWheelEvent { position: base, delta: ScrollDelta::Lines(point(0., if step < 90 { 1. } else { -1. })), ..Default::default() }.to_platform_input(), cx);
                            }
                        });
                        }
                    }
                    cycle += 1;
                }
                if elapsed >= (seconds + 5) as f64 {
                    let mut data = serde_json::json!({ "scene": scene, "backend": if cfg!(feature="gpui-fast") {"fast"} else {"default"},
                        "elapsed_s": elapsed, "sample_s": seconds, "metering": METERING.load(Ordering::Relaxed), "commands": COMMANDS.load(Ordering::Relaxed),
                        "windows": cx.windows().len(), "view_renders": RENDERS.load(Ordering::Relaxed) - renders_before, "frames": 0, "map_light": cx.global::<Core>().map_light });
                    if let Some(handle) = handle {
                        let _ = handle.update(cx, |_, window, _| {
                            let mut end = window.frame_duration_snapshot();
                            if let Some(before) = &before {
                                let _ = end.draw_duration_histogram.subtract(&before.draw_duration_histogram);
                                let _ = end.present_interval_histogram.subtract(&before.present_interval_histogram);
                            }
                            data["frames"] = end.draw_duration_histogram.len().into();
                            data["gpu_adapter"] = format!("{:?}", window.gpu_specs()).into();
                            data["draw_p50_ms"] = (end.draw_duration_histogram.value_at_quantile(0.5) as f64 / 1e6).into();
                            data["draw_p95_ms"] = (end.draw_duration_histogram.value_at_quantile(0.95) as f64 / 1e6).into();
                            data["present_interval_p50_ms"] = (end.present_interval_histogram.value_at_quantile(0.5) as f64 / 1e6).into();
                            if end.draw_duration_histogram.is_empty() {
                                data["draw_p50_ms"] = serde_json::Value::Null;
                                data["draw_p95_ms"] = serde_json::Value::Null;
                            }
                            if end.present_interval_histogram.is_empty() {
                                data["present_interval_p50_ms"] = serde_json::Value::Null;
                            }
                            data["width"] = f32::from(window.viewport_size().width).into();
                            data["height"] = f32::from(window.viewport_size().height).into();
                        });
                    }
                    eprintln!("BENCH_RESULT {data}");
                    cx.global_mut::<Core>().worker.stop();
                    cx.quit();
                }
            });
            if elapsed >= (seconds + 5) as f64 { break; }
        }
    }).detach();
}
