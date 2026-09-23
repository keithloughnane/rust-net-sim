//! Build automation for the Emergence workspace. Run `cargo xtask help` for the command list.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::error::Error;
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
  dist          Regenerate bindings and package a release build into dist/
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
        Some("dist") => dist(),
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

fn staticlib_name() -> String {
    if cfg!(windows) {
        format!("{LIB_NAME}.lib")
    } else {
        format!("lib{LIB_NAME}.a")
    }
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

fn dist() -> Result {
    bindings()?;
    build_native(true)?;

    let root = root();
    let out = root.join("dist");
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    let target = profile_dir(true);
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);

    // Plain C package: header plus shared and static libraries (Unreal, custom engines).
    let c_lib = out.join("c/lib").join(&platform);
    fs::create_dir_all(&c_lib)?;
    fs::create_dir_all(out.join("c/include"))?;
    fs::copy(
        root.join("bindings/c/emergence.h"),
        out.join("c/include/emergence.h"),
    )?;
    fs::copy(target.join(dylib_name()), c_lib.join(dylib_name()))?;
    fs::copy(target.join(staticlib_name()), c_lib.join(staticlib_name()))?;

    // Unity package: add via Package Manager > "Install package from disk" (package.json).
    let unity = out.join("unity/com.keithloughnane.emergence");
    copy_dir(&root.join("bindings/unity"), &unity)?;
    let unity_lib = unity.join("Runtime/Plugins").join(&platform);
    fs::create_dir_all(&unity_lib)?;
    fs::copy(target.join(dylib_name()), unity_lib.join(dylib_name()))?;

    println!("packaged {platform} build into {}", out.display());
    Ok(())
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
