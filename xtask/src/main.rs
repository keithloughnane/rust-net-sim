//! Build automation for the Emergence workspace. Run `cargo xtask help` for the command list.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
Usage: cargo xtask <command> [--release]

Commands:
  bindings      Regenerate bindings/c/emergence.h and the Unity C# bindings
  sandbox       Build the native library, then run the sandbox against it
  test-csharp   Build the native library, then run the C# binding smoke test (needs dotnet)
  dist          Regenerate bindings and package release builds for every platform into dist/
                (macOS universal, Linux x86-64, Windows x86-64; cross-compiling needs zig and
                cargo-zigbuild). `dist --host` packages only this machine's platform.
";

/// Name of the native library, matching `[lib] name` in `crates/emergence-ffi/Cargo.toml`.
const LIB_NAME: &str = "emergence";

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release");
    match args.first().map(String::as_str) {
        Some("bindings") => bindings(),
        Some("sandbox") => sandbox(release),
        Some("test-csharp") => test_csharp(release),
        Some("dist") => dist(args.iter().any(|a| a == "--host")),
        Some("help" | "--help" | "-h") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}").into()),
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn profile_dir(release: bool) -> PathBuf {
    root()
        .join("target")
        .join(if release { "release" } else { "debug" })
}

fn dylib_name() -> String {
    format!("{DLL_PREFIX}{LIB_NAME}{DLL_SUFFIX}")
}

fn cargo(args: &[&str], release: bool) -> Result {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root()).args(args);
    if release {
        cmd.arg("--release");
    }
    run_command(&mut cmd)
}

fn run_command(cmd: &mut Command) -> Result {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed ({status}): {cmd:?}").into())
    }
}

fn build_native(release: bool) -> Result {
    cargo(&["build", "-p", "emergence-ffi"], release)
}

fn bindings() -> Result {
    let root = root();
    let ffi = root.join("crates/emergence-ffi");

    let header = root.join("bindings/c/emergence.h");
    cbindgen::Builder::new()
        .with_crate(&ffi)
        .with_config(cbindgen::Config::from_file(ffi.join("cbindgen.toml"))?)
        .generate()?
        .write_to_file(&header);
    println!("wrote {}", header.display());

    let csharp = root.join("bindings/unity/Runtime/NativeMethods.g.cs");
    csbindgen::Builder::default()
        .input_extern_file(ffi.join("src/lib.rs"))
        .csharp_dll_name(LIB_NAME)
        // iOS links plugins statically, so P/Invoke must target the main executable.
        .csharp_dll_name_if("UNITY_IOS && !UNITY_EDITOR", "__Internal")
        .csharp_namespace("Emergence.Native")
        .csharp_class_name("NativeMethods")
        .csharp_class_accessibility("internal")
        .csharp_use_nint_types(false)
        .csharp_generate_const_filter(|name| name.starts_with("EMERGENCE_"))
        .generate_csharp_file(&csharp)?;
    println!("wrote {}", csharp.display());
    Ok(())
}

fn sandbox(release: bool) -> Result {
    build_native(release)?;
    cargo(&["run", "-p", "emergence-sandbox"], release)
}

fn test_csharp(release: bool) -> Result {
    build_native(release)?;
    let lib = profile_dir(release).join(dylib_name());
    run_command(
        Command::new("dotnet")
            .current_dir(root().join("bindings/csharp-smoke"))
            .arg("run")
            .arg(format!("-p:EmergenceNativeLib={}", lib.display())),
    )
}

/// A platform the packages ship a native library for.
struct Platform {
    /// Folder under `Runtime/Plugins` (Unity) and `c/lib` (C).
    folder: &'static str,
    /// Rust targets to build. Several are merged into one universal binary (macOS).
    targets: &'static [&'static str],
    /// Cross-compiled with cargo-zigbuild rather than built with cargo.
    zig: bool,
    /// The shared library's file name on this platform.
    file: &'static str,
    /// The static library's file name on this platform.
    static_file: &'static str,
    /// Unity plugin settings: the editor OS it loads in, the player it ships in, and the CPU.
    editor_os: &'static str,
    standalone: &'static str,
    cpu: &'static str,
    /// Fixed GUIDs for the Unity `.meta` files (folder, library), so repackaging never changes
    /// them and a project's references keep working.
    folder_guid: &'static str,
    file_guid: &'static str,
}

const PLATFORMS: &[Platform] = &[
    Platform {
        folder: "macos",
        targets: &["aarch64-apple-darwin", "x86_64-apple-darwin"],
        zig: false,
        file: "libemergence.dylib",
        static_file: "libemergence.a",
        editor_os: "OSX",
        standalone: "OSXUniversal",
        cpu: "AnyCPU",
        folder_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a40",
        file_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a41",
    },
    Platform {
        folder: "linux-x86_64",
        // glibc 2.17 or newer: practically every distro, and Steam's runtime.
        targets: &["x86_64-unknown-linux-gnu.2.17"],
        zig: true,
        file: "libemergence.so",
        static_file: "libemergence.a",
        editor_os: "Linux",
        standalone: "Linux64",
        cpu: "x86_64",
        folder_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a50",
        file_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a51",
    },
    Platform {
        folder: "windows-x86_64",
        targets: &["x86_64-pc-windows-gnu"],
        zig: true,
        file: "emergence.dll",
        static_file: "libemergence.a",
        editor_os: "Windows",
        standalone: "Win64",
        cpu: "x86_64",
        folder_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a60",
        file_guid: "6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a61",
    },
];

/// Where cargo puts a release build for `target` (zig's glibc suffix is not part of the path).
fn target_dir(target: &str) -> PathBuf {
    let triple = target.split_once(".2.").map_or(target, |(t, _)| t);
    root().join("target").join(triple).join("release")
}

/// Builds one platform's libraries and returns their paths (shared, static).
fn build_platform(platform: &Platform, out_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    for target in platform.targets {
        let tool = if platform.zig { "zigbuild" } else { "build" };
        cargo(&[tool, "-p", "emergence-ffi", "--target", target], true).map_err(|e| {
            format!(
                "building for {target} failed ({e}). Cross-compiling needs `brew install zig`, \
                 `cargo install cargo-zigbuild` and `rustup target add {}`; or run `cargo xtask \
                 dist --host` for this machine only.",
                target.split_once(".2.").map_or(*target, |(t, _)| t)
            )
        })?;
    }
    fs::create_dir_all(out_dir)?;
    let (shared, static_lib) = (
        out_dir.join(platform.file),
        out_dir.join(platform.static_file),
    );
    if let [only] = platform.targets {
        fs::copy(target_dir(only).join(platform.file), &shared)?;
        fs::copy(target_dir(only).join(platform.static_file), &static_lib)?;
    } else {
        // One universal binary: Unity refuses two plugins with the same name for one platform.
        for (file, dest) in [
            (platform.file, &shared),
            (platform.static_file, &static_lib),
        ] {
            let mut lipo = Command::new("lipo");
            lipo.arg("-create").arg("-output").arg(dest);
            for target in platform.targets {
                lipo.arg(target_dir(target).join(file));
            }
            run_command(&mut lipo)?;
        }
    }
    Ok((shared, static_lib))
}

fn dist(host_only: bool) -> Result {
    bindings()?;

    let root = root();
    let out = root.join("dist");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    let c_include = out.join("c/include");
    fs::create_dir_all(&c_include)?;
    fs::copy(
        root.join("bindings/c/emergence.h"),
        c_include.join("emergence.h"),
    )?;
    // Unity package: add via Package Manager > "Install package from disk" (package.json), or
    // copy it into a project's Packages folder.
    let unity = out.join("unity/com.keithloughnane.emergence");
    copy_dir(&root.join("bindings/unity"), &unity)?;
    let plugins = unity.join("Runtime/Plugins");

    let host = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows-x86_64"
    } else {
        "linux-x86_64"
    };
    for platform in PLATFORMS {
        if host_only && platform.folder != host {
            continue;
        }
        // Plain C package: header plus shared and static libraries (Unreal, custom engines).
        let (shared, _) = build_platform(platform, &out.join("c/lib").join(platform.folder))?;
        let unity_lib = plugins.join(platform.folder);
        fs::create_dir_all(&unity_lib)?;
        fs::copy(&shared, unity_lib.join(platform.file))?;
        fs::write(
            plugins.join(format!("{}.meta", platform.folder)),
            folder_meta(platform.folder_guid),
        )?;
        fs::write(
            unity_lib.join(format!("{}.meta", platform.file)),
            plugin_meta(platform),
        )?;
        println!("packaged {}", platform.folder);
    }
    fs::write(
        plugins.with_extension("meta"),
        folder_meta("6c1f0a8e2b7d4e9f8a3b5c7d9e1f2a30"),
    )?;
    println!("packaged into {}", out.display());
    Ok(())
}

/// A Unity `.meta` for a folder.
fn folder_meta(guid: &str) -> String {
    format!(
        "fileFormatVersion: 2\nguid: {guid}\nfolderAsset: yes\nDefaultImporter:\n  externalObjects: {{}}\n  \
         userData: \n  assetBundleName: \n  assetBundleVariant: \n"
    )
}

/// A Unity `.meta` making the library a native plugin for its own editor and player only.
fn plugin_meta(platform: &Platform) -> String {
    let standalones = ["Linux64", "OSXUniversal", "Win", "Win64"];
    let mut meta = format!(
        "fileFormatVersion: 2\nguid: {}\nPluginImporter:\n  externalObjects: {{}}\n  serializedVersion: 2\n  \
         iconMap: {{}}\n  executionOrder: {{}}\n  defineConstraints: []\n  isPreloaded: 0\n  \
         isOverridable: 1\n  isExplicitlyReferenced: 0\n  validateReferences: 1\n  platformData:\n  \
         - first:\n      : Any\n    second:\n      enabled: 0\n      settings:\n        Exclude Editor: 0\n",
        platform.file_guid
    );
    for s in standalones {
        let exclude = u8::from(s != platform.standalone);
        let _ = writeln!(meta, "        Exclude {s}: {exclude}");
    }
    meta += "  - first:\n      Any: \n    second:\n      enabled: 0\n      settings: {}\n";
    let _ = write!(
        meta,
        "  - first:\n      Editor: Editor\n    second:\n      enabled: 1\n      settings:\n        \
         CPU: {}\n        DefaultValueInitialized: true\n        OS: {}\n",
        platform.cpu, platform.editor_os
    );
    for s in standalones {
        let on = s == platform.standalone;
        let _ = write!(
            meta,
            "  - first:\n      Standalone: {s}\n    second:\n      enabled: {}\n      settings:\n        \
             CPU: {}\n",
            u8::from(on),
            if on { platform.cpu } else { "None" }
        );
    }
    meta + "  userData: \n  assetBundleName: \n  assetBundleVariant: \n"
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue; // .DS_Store and friends
        }
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
