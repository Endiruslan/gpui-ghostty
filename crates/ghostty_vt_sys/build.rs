use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("ghostty_vt_sys must live under crates/*");

    let ghostty_dir = workspace_root.join("vendor/ghostty");
    println!(
        "cargo:rerun-if-changed={}",
        ghostty_dir.join("build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("include/ghostty_vt.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/lib.zig").display()
    );

    if !ghostty_dir.exists() {
        panic!(
            "vendor/ghostty is missing; run `git submodule update --init --recursive` and retry"
        );
    }

    let zig = find_zig(workspace_root);
    let zig_version = Command::new(&zig).arg("version").output().ok();
    if zig_version.is_none() {
        panic!(
            "`zig` is required; run `./scripts/bootstrap-zig.sh` \
to install Zig 0.15.2 into .context/zig/zig"
        );
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let prefix = out_dir.join("zig-out");

    // zig <= 0.15 can't link against the macOS 26 SDK (libSystem.tbd parse
    // failure hits even zig's own build runner). Prepend the sdk-shim dir
    // so zig's `xcrun --show-sdk-path` lookup resolves to a macOS 15.x SDK
    // when one is installed — a no-op passthrough otherwise.
    let shim_dir = manifest_dir.join("sdk-shim");
    let path_env = {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut parts = vec![shim_dir.clone()];
        parts.extend(std::env::split_paths(&current));
        std::env::join_paths(parts).expect("join PATH")
    };
    println!("cargo:rerun-if-changed={}", shim_dir.join("xcrun").display());

    let status = Command::new(&zig)
        .current_dir(manifest_dir.join("zig"))
        .env("PATH", &path_env)
        .arg("build")
        .arg("-Doptimize=ReleaseFast")
        .arg("--prefix")
        .arg(&prefix)
        .status()
        .expect("failed to invoke zig");
    if !status.success() {
        panic!("zig build failed");
    }

    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=ghostty_vt");
    println!("cargo:rustc-link-lib=c");
}

fn find_zig(workspace_root: &std::path::Path) -> PathBuf {
    if let Some(path) = std::env::var_os("ZIG") {
        return PathBuf::from(path);
    }

    // Prefer the pinned toolchain: ghostty 1.3.x targets zig 0.15.x and
    // does NOT build with newer zig (0.16 removed @Type, changed build
    // APIs), so a system-wide `zig` from PATH is usually the wrong one.
    let pinned = workspace_root.join(".context/zig/zig");
    if pinned.exists() {
        return pinned;
    }

    if Command::new("zig").arg("version").output().is_ok() {
        return PathBuf::from("zig");
    }

    pinned
}
