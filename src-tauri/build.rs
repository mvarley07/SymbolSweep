use std::process::Command;

fn main() {
    // Stamp git short-SHA at build time so the binary knows which commit it came from.
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let dirty = Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);

    let build_sha = if dirty {
        format!("{}+", sha)
    } else {
        sha
    };

    println!("cargo:rustc-env=BUILD_SHA={}", build_sha);
    // Re-run if HEAD changes (new commit) or index changes (dirty toggle)
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    tauri_build::build()
}
