//! Builds the native PipeWire audio graph (native/audio_graph.c).
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/audio_graph.c");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join("audio_graph.o");
    let flags = Command::new("pkg-config")
        .args(["--cflags", "libpipewire-0.3 >= 1.0"])
        .output()
        .expect("pkg-config required");
    assert!(flags.status.success(), "PipeWire development headers >= 1.0 required");
    let status = Command::new("cc")
        .args(["-std=gnu11", "-D_GNU_SOURCE", "-O2", "-Wall", "-Wextra", "-Werror", "-fPIC", "-c"])
        .arg("native/audio_graph.c")
        .arg("-o")
        .arg(&object)
        .args(std::str::from_utf8(&flags.stdout).unwrap().split_whitespace())
        .status()
        .expect("C compiler required");
    assert!(status.success(), "audio graph compilation failed");
    assert!(Command::new("ar")
        .arg("crs")
        .arg(out.join("libwavelane_graph.a"))
        .arg(&object)
        .status()
        .unwrap()
        .success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=wavelane_graph");
    println!("cargo:rustc-link-lib=dylib:+verbatim=libpipewire-0.3.so.0");
    println!("cargo:rustc-link-lib=m");
}
