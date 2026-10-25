#[cfg(feature = "gpui-fast")]
extern crate gpui_kit as gpui;
#[cfg(feature = "gpui-fast")]
extern crate wavelane_fast_linux as wavelane_linux;
#[cfg(not(feature = "gpui-fast"))]
extern crate gpui_linux as wavelane_linux;
#[cfg(feature = "perf-harness")]
mod benchmark;
// Wavelane: drag each application's sound onto any output.
mod app;
mod app_icons;
mod engine;
mod flyout;
mod graph;
mod hotkey;
mod map;
mod single_instance;
mod tray;
mod updates;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{} {}", app::title(), env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help") {
        println!(
            "Wavelane: see where every application's sound goes, and drag it onto any output.\n\
             --map      open or close the map (bind this to a key, or use the shortcut)\n\
             --hidden   start in the tray (login autostart)\n\
             --dump     print what the engine sees and exit\n\
             --version  print the version"
        );
        return;
    }
    if args.first().map(String::as_str) == Some("--dump") {
        dump();
        return;
    }
    #[cfg(not(feature = "perf-harness"))]
    if args.iter().any(|a| a == "--benchmark") {
        eprintln!("This build does not include the isolated benchmark harness.");
        std::process::exit(2);
    }
    #[cfg(feature = "perf-harness")]
    if benchmark::requested() {
        app::run(true, None);
        return;
    }
    let hidden = args.iter().any(|a| a == "--hidden");
    let map = args.iter().any(|a| a == "--map");
    // Before anything touches PipeWire: a later launch only asks the running
    // Wavelane to open (or close) the map.
    let instance = match single_instance::claim(hidden, map) {
        Ok(single_instance::Claim::Primary(instance)) => Some(instance),
        Ok(single_instance::Claim::Running(Ok(()))) => return,
        Ok(single_instance::Claim::Running(Err(error))) => {
            eprintln!("Wavelane is already running but did not answer: {error}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Wavelane single-instance check unavailable: {error}");
            None
        }
    };
    app::run(hidden, instance);
}

/// Diagnostics: what the engine sees, once a second, three times.
fn dump() {
    let worker = engine::Worker::spawn();
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let view = worker.view();
        println!("phase {:?} default {:?} error {:?}", view.phase, view.default_output, view.error);
        for o in &view.outputs {
            println!("  output {} | {} | device {} {:?}", o.name, o.description, o.device, o.device_description);
        }
        for a in &view.apps {
            println!("  app {:?} on {:?} pinned {:?} vol {:?} mute {}", a.key, a.playing_on, a.pinned, a.volume, a.mute);
        }
    }
}
