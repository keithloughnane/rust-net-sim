//! Loads the Emergence Engine as a native shared library through its C ABI.
//!
//! This deliberately does not link the engine as a Rust crate. It opens the same
//! `libemergence` binary a game engine would, looks up the exported symbols, and mirrors the
//! declarations in `bindings/c/emergence.h` by hand, the way the Unity bindings do. If the
//! sandbox works, the shipped library and its ABI work.

#![allow(unsafe_code)] // Calling into a foreign library is unsafe by nature; keep it in this module.

use std::ffi::{CStr, c_char};
use std::fmt;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use libloading::Library;

/// ABI version this module was written against (`EMERGENCE_ABI_VERSION` in the header).
const EXPECTED_ABI_VERSION: u32 = 1;

/// Environment variable that overrides where the library is loaded from.
const LIB_PATH_VAR: &str = "EMERGENCE_LIB";

/// `EmergenceStatus` from the header.
type Status = u32;
const STATUS_OK: Status = 0;
const STATUS_NULL_POINTER: Status = 1;
const STATUS_PANIC: Status = 2;

/// Opaque `EmergenceWorld` from the header.
#[repr(C)]
struct RawWorld {
    _private: [u8; 0],
}

/// Something went wrong loading or calling the native library.
#[derive(Debug)]
pub(crate) enum NativeError {
    Load {
        path: PathBuf,
        source: libloading::Error,
    },
    AbiMismatch {
        found: u32,
    },
    Status(Status),
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load { path, source } => {
                write!(f, "could not load {}: {source}", path.display())
            }
            Self::AbiMismatch { found } => write!(
                f,
                "library ABI version {found} does not match expected {EXPECTED_ABI_VERSION}"
            ),
            Self::Status(STATUS_NULL_POINTER) => f.write_str("native call received a null pointer"),
            Self::Status(STATUS_PANIC) => f.write_str("native library hit an internal error"),
            Self::Status(other) => write!(f, "native call returned unknown status {other}"),
        }
    }
}

impl std::error::Error for NativeError {}

fn check(status: Status) -> Result<(), NativeError> {
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(NativeError::Status(status))
    }
}

/// The loaded native library and the function pointers resolved from it.
pub(crate) struct NativeLibrary {
    path: PathBuf,
    version: String,
    world_create: unsafe extern "C" fn(*mut *mut RawWorld) -> Status,
    world_destroy: unsafe extern "C" fn(*mut RawWorld),
    world_tick: unsafe extern "C" fn(*mut RawWorld) -> Status,
    world_tick_count: unsafe extern "C" fn(*const RawWorld, *mut u64) -> Status,
    // Declared last so it is dropped (unloaded) after nothing else can use the pointers above.
    _library: Library,
}

impl fmt::Debug for NativeLibrary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeLibrary")
            .field("path", &self.path)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl NativeLibrary {
    /// Default location: next to the sandbox executable, where Cargo puts both builds.
    /// Overridden by the `EMERGENCE_LIB` environment variable.
    pub(crate) fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os(LIB_PATH_VAR) {
            return path.into();
        }
        let file_name = libloading::library_filename("emergence");
        std::env::current_exe().map_or_else(
            |_| file_name.clone().into(),
            |exe| exe.with_file_name(&file_name),
        )
    }

    /// Loads the library at `path`, resolves every symbol, and checks the ABI version.
    pub(crate) fn load(path: &Path) -> Result<Arc<Self>, NativeError> {
        let load_err = |source| NativeError::Load {
            path: path.to_path_buf(),
            source,
        };

        // SAFETY: loading runs the library's initializers; libemergence has none beyond Rust's.
        let library = unsafe { Library::new(path) }.map_err(load_err)?;

        // SAFETY: every signature below matches the corresponding declaration in emergence.h.
        unsafe {
            let abi_version = *library
                .get::<extern "C" fn() -> u32>(b"emergence_abi_version\0")
                .map_err(load_err)?;
            let found = abi_version();
            if found != EXPECTED_ABI_VERSION {
                return Err(NativeError::AbiMismatch { found });
            }

            let version = *library
                .get::<extern "C" fn() -> *const c_char>(b"emergence_version\0")
                .map_err(load_err)?;
            let version = CStr::from_ptr(version()).to_string_lossy().into_owned();

            Ok(Arc::new(Self {
                path: path.to_path_buf(),
                version,
                world_create: *library.get(b"emergence_world_create\0").map_err(load_err)?,
                world_destroy: *library
                    .get(b"emergence_world_destroy\0")
                    .map_err(load_err)?,
                world_tick: *library.get(b"emergence_world_tick\0").map_err(load_err)?,
                world_tick_count: *library
                    .get(b"emergence_world_tick_count\0")
                    .map_err(load_err)?,
                _library: library,
            }))
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }
}

/// A world living inside the native library. Destroyed when dropped.
#[derive(Debug)]
pub(crate) struct NativeWorld {
    library: Arc<NativeLibrary>,
    handle: NonNull<RawWorld>,
}

impl NativeWorld {
    pub(crate) fn new(library: Arc<NativeLibrary>) -> Result<Self, NativeError> {
        let mut raw = ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer.
        check(unsafe { (library.world_create)(&raw mut raw) })?;
        let handle = NonNull::new(raw).ok_or(NativeError::Status(STATUS_NULL_POINTER))?;
        Ok(Self { library, handle })
    }

    pub(crate) fn tick(&mut self) -> Result<(), NativeError> {
        // SAFETY: `handle` is live until drop, and `&mut self` guarantees exclusive use.
        check(unsafe { (self.library.world_tick)(self.handle.as_ptr()) })
    }

    pub(crate) fn tick_count(&self) -> Result<u64, NativeError> {
        let mut count = 0;
        // SAFETY: `handle` is live until drop; `count` is a valid out-pointer.
        check(unsafe { (self.library.world_tick_count)(self.handle.as_ptr(), &raw mut count) })?;
        Ok(count)
    }
}

impl Drop for NativeWorld {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `world_create` and is destroyed exactly once, here.
        unsafe { (self.library.world_destroy)(self.handle.as_ptr()) };
    }
}
