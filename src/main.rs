//! Wavelane: drag each application's sound onto any output.
mod engine;
mod graph;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--dump") {
        // Diagnostics: what the engine sees, once a second, three times.
        let worker = engine::Worker::spawn();
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let view = worker.view();
            println!("phase {:?} default {:?} error {:?}", view.phase, view.default_output, view.error);
            for o in &view.outputs {
                println!("  output {} | {} | device {} {:?} | vol {:?}", o.name, o.description, o.device, o.device_description, o.volume);
            }
            for a in &view.apps {
                println!("  app {:?} on {:?} pinned {:?} vol {:?} mute {} running {}", a.key, a.playing_on, a.pinned, a.volume, a.mute, a.running);
            }
        }
        return;
    }
    eprintln!("Wavelane: the interface is not built yet; try --dump");
}
