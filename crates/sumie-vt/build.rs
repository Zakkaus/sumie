use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GHOSTTY_COMMIT: &str = "108cacb17ed8990840cd14abfce43d8ed8503ca1";
const GHOSTTY_ARCHIVE_SHA256: &str =
    "2c276d3e62cb15fb38f0d7f621f199029d1d4df86538096c6bec2a107cad7159";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    for var in [
        "ZIG",
        "PATH",
        "SUMIE_GHOSTTY_ARCHIVE",
        "GHOSTTY_ZIG_SYSTEM_DIR",
        "ZIG_GLOBAL_CACHE_DIR",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }

    assert_eq!(
        env::var("HOST"),
        env::var("TARGET"),
        "sumie-vt requires a native build"
    );
    let zig = zig();
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let source = source(&out);
    resolve_cursor_default_at_snapshot(&source);
    let prefix = out.join("ghostty-install");
    let mut build = Command::new(zig);
    build
        .current_dir(source)
        .args([
            "build",
            "-Demit-lib-vt=true",
            "-Demit-xcframework=false",
            "-Dapp-runtime=none",
            "-Dlib-version-string=1.0.1",
            "-Doptimize=ReleaseSafe",
            "-Dcpu=baseline",
        ])
        .arg("--prefix")
        .arg(&prefix)
        .arg("--cache-dir")
        .arg(out.join("zig-cache"));
    if let Some(dir) = env::var_os("GHOSTTY_ZIG_SYSTEM_DIR") {
        build
            .arg("--system")
            .arg(std::path::absolute(dir).expect("resolve GHOSTTY_ZIG_SYSTEM_DIR"));
    }
    run(&mut build, "build libghostty-vt with Zig");
    println!("cargo::warning=Built libghostty-vt vt-1.0.1 ({GHOSTTY_COMMIT})");
    println!(
        "cargo::rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo::rustc-link-lib=static=ghostty-vt");
}

fn resolve_cursor_default_at_snapshot(source: &Path) {
    // The pinned Ghostty stores each screen's last shape, but default-following
    // is terminal-wide. Resolve the default without changing the shared blink mode.
    const VISUAL_STYLE: &str = "self.cursor.visual_style = s.cursor.cursor_style;";
    const FIXED: &str = "self.cursor.visual_style = if (t.cursor.is_default)\n            \
        t.cursor.default_style\n        else\n            s.cursor.cursor_style;";
    let path = source.join("src/terminal/render.zig");
    let text = fs::read_to_string(&path).expect("read pinned Ghostty render.zig");
    if !text.contains(FIXED) {
        assert_eq!(
            text.matches(VISUAL_STYLE).count(),
            1,
            "Ghostty patch anchor"
        );
        fs::write(path, text.replacen(VISUAL_STYLE, FIXED, 1)).expect("patch Ghostty cursor shape");
    }
}

fn zig() -> PathBuf {
    let zig = PathBuf::from(env::var_os("ZIG").unwrap_or_else(|| "zig".into()));
    let zig = if zig.is_relative() && zig.parent().is_some_and(|path| path != Path::new("")) {
        std::path::absolute(zig).expect("resolve ZIG")
    } else {
        zig
    };
    let output = Command::new(&zig)
        .arg("version")
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "sumie-vt requires Zig 0.17; install it on PATH or set ZIG: {}: {err}",
                zig.display()
            )
        });
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && version.trim().starts_with("0.17."),
        "sumie-vt requires Zig 0.17; {} version returned {}: {version}{}",
        zig.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    zig
}

fn source(out: &Path) -> PathBuf {
    let source = out.join(format!("ghostty-{GHOSTTY_COMMIT}"));
    if source.is_dir() {
        return source;
    }

    let download = out.join("ghostty.tar.gz");
    let archive = if let Some(path) = env::var_os("SUMIE_GHOSTTY_ARCHIVE") {
        PathBuf::from(path)
    } else {
        run(
            Command::new("curl")
                .args(["-fsSL", "--output"])
                .arg(&download)
                .arg(format!(
                    "https://codeload.github.com/ghostty-org/ghostty/tar.gz/{GHOSTTY_COMMIT}"
                )),
            "download the pinned Ghostty source",
        );
        download.clone()
    };
    let digest = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&archive)
        .output()
        .expect("run shasum to verify the pinned Ghostty source");
    assert!(
        digest.status.success()
            && String::from_utf8_lossy(&digest.stdout)
                .split_whitespace()
                .next()
                == Some(GHOSTTY_ARCHIVE_SHA256),
        "Ghostty source checksum mismatch for {}: {}",
        archive.display(),
        String::from_utf8_lossy(&digest.stderr),
    );

    let staging = out.join("ghostty-unpack");
    if staging.exists() {
        fs::remove_dir_all(&staging).expect("remove incomplete Ghostty extraction");
    }
    fs::create_dir(&staging).expect("create Ghostty extraction directory");
    run(
        Command::new("tar")
            .args(["-xzf"])
            .arg(&archive)
            .arg("--strip-components=1")
            .arg("-C")
            .arg(&staging),
        "extract the pinned Ghostty source",
    );
    fs::rename(staging, &source).expect("install verified Ghostty source");
    if archive == download {
        fs::remove_file(download).expect("remove extracted Ghostty archive");
    }
    source
}

fn run(command: &mut Command, context: &str) {
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("{context}: {err}"));
    assert!(status.success(), "{context}: {status}");
}
