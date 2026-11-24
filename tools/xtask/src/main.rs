//! The project's build system: one command, the same on every platform.
//!
//!     cargo xtask build [--release]
//!     cargo xtask flatpak
//!
//! `build` tests the core and builds everything that can be built on this
//! machine: the Rust core always, plus the front ends for the host platform
//! (the macOS and iOS apps on a Mac, the GTK app on Linux). New front ends
//! (Plasma, Windows) get their steps added in `build` below, behind the
//! right `cfg!`. `flatpak` builds the GTK app as a Flatpak and installs it
//! for the current user.

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
        "flatpak" => flatpak(),
        "cargo-sources" => write_cargo_sources(),
        "help" => {
            println!("usage: cargo xtask [build|test|flatpak] [--release]\n\n  build    test the core, then build it and every front end for this host (default)\n  test     run the core's tests\n  flatpak  build the GTK app as a Flatpak and install it for this user");
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
    if cfg!(target_os = "linux") {
        gtk(release)?;
    }
    // Linux: the Plasma front end goes here. Windows: the WinUI one.

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

/// The GTK app: natively when the development packages are installed,
/// otherwise as a Flatpak when flatpak-builder is at hand.
fn gtk(release: bool) -> Result {
    let root = root();
    let libraries = ["gtk4", "libadwaita-1", "webkitgtk-6.0", "poppler-glib"];
    let missing: Vec<&str> = libraries
        .iter()
        .copied()
        .filter(|lib| !Command::new("pkg-config").args(["--exists", lib]).status().is_ok_and(|s| s.success()))
        .collect();
    if missing.is_empty() {
        let profile = if release { "release" } else { "debug" };
        step(&format!("Building the GTK app ({profile})"));
        let mut args = vec!["build", "--package", "rattonero-gtk"];
        if release {
            args.push("--release");
        }
        run(&cargo(), &args, &root, &[])?;
        println!("\nGTK app: target/{profile}/rattonero-books");
        return Ok(());
    }
    if flatpak_builder().is_some() {
        println!("\n(no development packages for {}; building the Flatpak instead)", missing.join(", "));
        return flatpak();
    }
    println!(
        "\nSkipping the GTK app: install the development packages for {} \
         or flatpak-builder (`flatpak install flathub org.flatpak.Builder`).",
        missing.join(", ")
    );
    Ok(())
}

const FLATPAK_ID: &str = "com.rattonero.ebookreader";

/// flatpak-builder, installed natively or as the Flathub app.
fn flatpak_builder() -> Option<Vec<&'static str>> {
    let works = |program: &str, args: &[&str]| {
        Command::new(program).args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
    };
    if works("flatpak-builder", &["--version"]) {
        Some(vec!["flatpak-builder"])
    } else if works("flatpak", &["info", "org.flatpak.Builder"]) {
        Some(vec!["flatpak", "run", "org.flatpak.Builder"])
    } else {
        None
    }
}

/// Build the GTK app as a Flatpak and install it for the current user. The
/// runtime, SDK and Rust extension come from Flathub when missing.
fn flatpak() -> Result {
    let root = root();
    write_cargo_sources()?;
    // Fill cargo's own cache first; then flatpak-builder needs no network
    // for crates at all.
    let _ = run(&cargo(), &["fetch", "--quiet", "--locked"], &root, &[]);
    seed_flatpak_downloads(&root)?;

    step("Building and installing the Flatpak");
    let builder = flatpak_builder()
        .ok_or("flatpak-builder not found; `flatpak install flathub org.flatpak.Builder` installs it")?;
    let manifest = format!("flatpak/{FLATPAK_ID}.yml");
    let mut args: Vec<&str> = builder[1..].to_vec();
    args.extend([
        "--user",
        "--install",
        "--force-clean",
        "--install-deps-from=flathub",
        // rofiles-fuse cannot run inside the Flathub build of flatpak-builder.
        "--disable-rofiles-fuse",
        "--state-dir=target/flatpak/state",
        "target/flatpak/build",
        &manifest,
    ]);
    run(builder[0], &args, &root, &[])?;
    println!("\nInstalled. Start it from the app grid or with `flatpak run {FLATPAK_ID}`.");
    Ok(())
}

struct Crate {
    name: String,
    version: String,
    checksum: String,
}

/// The crates.io packages in Cargo.lock, with their checksums.
fn registry_crates(lock: &str) -> Vec<Crate> {
    let mut crates = Vec::new();
    for package in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            package.lines().find_map(|line| {
                let value = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
                Some(value.trim_matches('"').to_string())
            })
        };
        let (Some(name), Some(version), Some(source), Some(checksum)) =
            (field("name"), field("version"), field("source"), field("checksum"))
        else {
            continue; // workspace members have neither source nor checksum
        };
        if source == "registry+https://github.com/rust-lang/crates.io-index" {
            crates.push(Crate { name, version, checksum });
        }
    }
    crates
}

/// Hand flatpak-builder the crates cargo has already downloaded, so that it
/// fetches only what is missing. Its download cache is keyed by checksum,
/// the same one Cargo.lock records, so a stale file can never slip in.
fn seed_flatpak_downloads(root: &Path) -> Result {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let Some(registry) = cargo_home.map(|home| home.join("registry/cache")) else { return Ok(()) };
    let caches: Vec<PathBuf> = fs::read_dir(&registry).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    let lock = fs::read_to_string(root.join("Cargo.lock")).map_err(|e| e.to_string())?;
    let downloads = root.join("target/flatpak/state/downloads");
    let mut seeded = 0;
    for Crate { name, version, checksum } in registry_crates(&lock) {
        let file = format!("{name}-{version}.crate");
        let target = downloads.join(&checksum).join(&file);
        if target.exists() {
            continue;
        }
        if let Some(cached) = caches.iter().map(|c| c.join(&file)).find(|p| p.exists()) {
            fs::create_dir_all(target.parent().expect("a parent")).map_err(|e| e.to_string())?;
            if fs::hard_link(&cached, &target).is_err() {
                fs::copy(&cached, &target).map_err(|e| e.to_string())?;
            }
            seeded += 1;
        }
    }
    if seeded > 0 {
        println!("({seeded} crates taken from the local cargo cache)");
    }
    Ok(())
}

/// `flatpak/cargo-sources.json`, which the manifest needs (also for Flathub).
fn write_cargo_sources() -> Result {
    step("Listing the crates for an offline Flatpak build");
    let root = root();
    let sources = cargo_sources(&fs::read_to_string(root.join("Cargo.lock")).map_err(|e| e.to_string())?);
    fs::write(root.join("flatpak/cargo-sources.json"), sources).map_err(|e| e.to_string())
}

/// Every registry crate in Cargo.lock as flatpak-builder sources, unpacked
/// into a vendor directory that `cargo/config.toml` points cargo at. The
/// same thing flatpak-cargo-generator.py produces, without needing Python.
fn cargo_sources(lock: &str) -> String {
    let mut entries = Vec::new();
    for Crate { name, version, checksum } in registry_crates(lock) {
        let dest = format!("cargo/vendor/{name}-{version}");
        entries.push(format!(
            "  {{\"type\": \"archive\", \"archive-type\": \"tar-gzip\", \
             \"url\": \"https://static.crates.io/crates/{name}/{name}-{version}.crate\", \
             \"sha256\": \"{checksum}\", \"dest\": \"{dest}\"}}"
        ));
        entries.push(format!(
            "  {{\"type\": \"inline\", \"contents\": \"{{\\\"package\\\": \\\"{checksum}\\\", \\\"files\\\": {{}}}}\", \
             \"dest\": \"{dest}\", \"dest-filename\": \".cargo-checksum.json\"}}"
        ));
    }
    entries.push(
        "  {\"type\": \"inline\", \"contents\": \"[source.vendored-sources]\\ndirectory = \\\"cargo/vendor\\\"\\n\\n\
         [source.crates-io]\\nreplace-with = \\\"vendored-sources\\\"\\n\", \
         \"dest\": \"cargo\", \"dest-filename\": \"config.toml\"}"
            .to_string(),
    );
    format!("[\n{}\n]\n", entries.join(",\n"))
}
