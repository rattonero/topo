//! The project's build system: one command, the same on every platform.
//!
//!     cargo xtask build [--release]
//!
//! tests the core and builds everything that can be built on this machine:
//! the Rust core always, plus the front ends for the host platform (today
//! the macOS and iOS apps on a Mac). New front ends (GTK4, Plasma, Windows)
//! get their steps added in `build` below, behind the right `cfg!`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

type Result<T = ()> = std::result::Result<T, String>;

const MACOS_DEPLOYMENT_TARGET: &str = "14.0";
const IOS_DEPLOYMENT_TARGET: &str = "17.0";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    let command = args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or("build");
    let result = match command {
        "build" => build(release),
        "test" => test(),
        "help" => {
            println!("usage: cargo xtask [build|test] [--release]\n\n  build   test the core, then build it and every front end for this host (default)\n  test    run the core's tests");
            Ok(())
        }
        other => Err(format!("unknown command `{other}`; try `cargo xtask help`")),
    };
    if let Err(message) = result {
        eprintln!("\nerror: {message}");
        exit(1);
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("workspace root").to_path_buf()
}

fn step(title: &str) {
    println!("\n==> {title}");
}

/// Run a program from the workspace root, failing with a readable message.
fn run(program: &str, args: &[&str], dir: &Path, envs: &[(&str, &str)]) -> Result {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .envs(envs.iter().copied())
        .status()
        .map_err(|e| format!("could not run `{program}` ({e}); is it installed?"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` failed", args.join(" ")))
    }
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn test() -> Result {
    step("Testing the core");
    run(&cargo(), &["test", "--quiet", "--package", "rattonero-core"], &root(), &[])
}

fn build(release: bool) -> Result {
    test()?;
    let profile = if release { "release" } else { "debug" };

    step(&format!("Building the core for this machine ({profile})"));
    let mut args = vec!["build", "--package", "rattonero-core"];
    if release {
        args.push("--release");
    }
    run(&cargo(), &args, &root(), &[])?;

    if cfg!(target_os = "macos") {
        apple(release)?;
    }
    // Linux: GTK4 and Plasma front ends go here. Windows: the WinUI one.

    step("Done");
    Ok(())
}

/// Core for Apple targets, Swift bindings, XCFramework, then both apps.
fn apple(release: bool) -> Result {
    let root = root();
    let profile = if release { "release" } else { "debug" };
    let configuration = if release { "Release" } else { "Debug" };
    let lib = "librattonero_core.a";
    let deployment = [
        ("MACOSX_DEPLOYMENT_TARGET", MACOS_DEPLOYMENT_TARGET),
        ("IPHONEOS_DEPLOYMENT_TARGET", IOS_DEPLOYMENT_TARGET),
    ];

    // Release builds also run on Intel Macs.
    let mut targets = vec!["aarch64-apple-darwin", "aarch64-apple-ios", "aarch64-apple-ios-sim"];
    if release {
        targets.push("x86_64-apple-darwin");
    }
    for target in &targets {
        step(&format!("Building the core for {target} ({profile})"));
        let mut args = vec!["build", "--package", "rattonero-core", "--target", target];
        if release {
            args.push("--release");
        }
        run(&cargo(), &args, &root, &deployment)
            .map_err(|e| format!("{e}\nhint: `rustup target add {target}` installs a missing target"))?;
    }

    step("Generating the Swift bindings");
    let generated = root.join("apple/Generated");
    let headers = root.join("target/apple-headers");
    let framework = root.join("apple/RattoneroCore.xcframework");
    for dir in [&generated, &headers, &framework] {
        let _ = fs::remove_dir_all(dir);
    }
    for dir in [&generated, &headers] {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let dylib = format!("target/aarch64-apple-darwin/{profile}/librattonero_core.dylib");
    run(
        &cargo(),
        &["run", "--quiet", "--package", "uniffi-bindgen", "--", "generate", "--library", &dylib, "--language", "swift", "--out-dir", "apple/Generated"],
        &root,
        &[],
    )?;
    // Xcode wants the C header and module map beside the library, and the
    // Swift file in the app's sources.
    for entry in fs::read_dir(&generated).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let destination = match path.extension().and_then(|e| e.to_str()) {
            Some("h") => headers.join(entry.file_name()),
            Some("modulemap") => headers.join("module.modulemap"),
            Some("swift") => generated.join("RattoneroCore.swift"),
            _ => continue,
        };
        fs::rename(&path, destination).map_err(|e| e.to_string())?;
    }

    step("Packaging RattoneroCore.xcframework");
    let mut mac_lib = format!("target/aarch64-apple-darwin/{profile}/{lib}");
    if release {
        fs::create_dir_all(root.join("target/apple-universal")).map_err(|e| e.to_string())?;
        let universal = format!("target/apple-universal/{lib}");
        let intel = format!("target/x86_64-apple-darwin/release/{lib}");
        run("lipo", &["-create", &mac_lib, &intel, "-output", &universal], &root, &[])?;
        mac_lib = universal;
    }
    let ios_lib = format!("target/aarch64-apple-ios/{profile}/{lib}");
    let sim_lib = format!("target/aarch64-apple-ios-sim/{profile}/{lib}");
    let headers_arg = "target/apple-headers";
    run(
        "xcodebuild",
        &[
            "-create-xcframework",
            "-library", &mac_lib, "-headers", headers_arg,
            "-library", &ios_lib, "-headers", headers_arg,
            "-library", &sim_lib, "-headers", headers_arg,
            "-output", "apple/RattoneroCore.xcframework",
        ],
        &root,
        &[],
    )?;

    step("Generating the Xcode project");
    run("xcodegen", &["generate", "--quiet"], &root.join("apple"), &[])
        .map_err(|e| format!("{e}\nhint: `brew install xcodegen`"))?;

    for (name, destination) in [("macOS", "platform=macOS"), ("iOS Simulator", "generic/platform=iOS Simulator")] {
        step(&format!("Building the {name} app ({configuration})"));
        run(
            "xcodebuild",
            &[
                "-project", "apple/Rattonero.xcodeproj", "-scheme", "Rattonero",
                "-configuration", configuration, "-destination", destination,
                "-derivedDataPath", "target/xcode", "-quiet", "build",
            ],
            &root,
            &[],
        )?;
    }
    // macOS remembers an app's icon by path. Without this, the Dock keeps
    // showing whatever icon (or none) the first build at this path had.
    let mac_app = format!("target/xcode/Build/Products/{configuration}/Rattonero Books.app");
    run("touch", &[&mac_app], &root, &[])?;
    let lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    let _ = run(lsregister, &["-f", &mac_app], &root, &[]);

    println!("\nmacOS app:         {mac_app}");
    println!("iOS simulator app: target/xcode/Build/Products/{configuration}-iphonesimulator/Rattonero Books.app");
    Ok(())
}
