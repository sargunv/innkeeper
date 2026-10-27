//! Compiles the in-session icon helper (`android/IconDump.java`) to a dex jar
//! and embeds it, so `linuxapk` is one self-contained binary. Needs `javac` and
//! the Android SDK (`$ANDROID_HOME`, default `~/Android/Sdk`: a platform
//! `android.jar` and build-tools `d8`). Without them the binary is still built,
//! with an empty helper: icons then come from the APK's PNGs only.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = "android/IconDump.java";

fn newest(dir: &Path, file: &str) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path().join(file)))
        .filter(|p| p.is_file())
        .collect();
    // Version-ish sort: "android-37.0" > "android-36.1" > "android-36".
    v.sort_by_key(|p| {
        let name = p.parent().unwrap().file_name().unwrap().to_string_lossy();
        name.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    v.pop()
}

fn build(out: &Path) -> Result<PathBuf, String> {
    let sdk = env::var_os("ANDROID_HOME")
        .or_else(|| env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join("Android/Sdk")))
        .ok_or("no ANDROID_HOME")?;
    let jar = newest(&sdk.join("platforms"), "android.jar").ok_or("no platforms/*/android.jar")?;
    let d8 = newest(&sdk.join("build-tools"), "d8").ok_or("no build-tools/*/d8")?;
    let classes = out.join("classes");
    let _ = fs::remove_dir_all(&classes);
    fs::create_dir_all(&classes).map_err(|e| e.to_string())?;
    let st = Command::new("javac")
        .args(["--release", "11", "-nowarn", "-cp"])
        .arg(&jar)
        .arg("-d")
        .arg(&classes)
        .arg(SRC)
        .status()
        .map_err(|e| format!("javac: {e}"))?;
    if !st.success() {
        return Err("javac failed".into());
    }
    let dex = out.join("linuxapk-icon.jar");
    let _ = fs::remove_file(&dex);
    let mut cmd = Command::new(&d8);
    cmd.args(["--release", "--min-api", "30", "--lib"])
        .arg(&jar)
        .arg("--output")
        .arg(&dex);
    for e in walk(&classes) {
        cmd.arg(e);
    }
    let st = cmd.status().map_err(|e| format!("d8: {e}"))?;
    if !st.success() || !dex.is_file() {
        return Err("d8 failed".into());
    }
    Ok(dex)
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                v.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "class") {
                v.push(p);
            }
        }
    }
    v
}

fn main() {
    println!("cargo::rerun-if-changed={SRC}");
    println!("cargo::rerun-if-env-changed=ANDROID_HOME");
    println!("cargo::rerun-if-env-changed=ANDROID_SDK_ROOT");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let dst = out.join("icon-helper.jar");
    match build(&out) {
        Ok(dex) => fs::copy(dex, &dst).map(|_| ()).unwrap(),
        Err(e) => {
            println!("cargo::warning=icon helper not built ({e}); icons fall back to APK PNGs");
            fs::write(&dst, b"").unwrap();
        }
    }
}
