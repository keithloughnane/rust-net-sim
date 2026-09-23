//! Loads the Emergence Engine as a native shared library through its C ABI.
//!
//! This deliberately does not link the engine as a Rust crate. It opens the same
//! `libemergence` binary a game engine would, looks up the exported symbols, and mirrors the
//! declarations in `bindings/c/emergence.h` by hand, the way the Unity bindings do. If the
//! sandbox works, the shipped library and its ABI work.

#![allow(unsafe_code)] // Calling into a foreign library is unsafe by nature; keep it in this module.

use std::ffi::{CStr, CString, NulError, c_char};
use std::fmt;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use libloading::Library;
use serde::Deserialize;

use crate::snapshot::{Snapshot, SnapshotError};

/// ABI version this module was written against (`EMERGENCE_ABI_VERSION` in the header).
const EXPECTED_ABI_VERSION: u32 = 2;

/// Environment variable that overrides where the library is loaded from.
const LIB_PATH_VAR: &str = "EMERGENCE_LIB";

/// `EmergenceStatus` from the header. Kept as a plain integer: a newer library may return codes
/// this module does not know.
type Status = u32;
const STATUS_OK: Status = 0;

/// `EmergenceNodeId` from the header. Also how node IDs appear in the JSON snapshot.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(transparent)]
pub(crate) struct NodeId {
    raw: u64,
}

/// `EmergenceLinkId` from the header. Also how link IDs appear in the JSON snapshot.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(transparent)]
pub(crate) struct LinkId {
    raw: u64,
}

impl NodeId {
    pub(crate) fn raw(self) -> u64 {
        self.raw
    }
}

impl LinkId {
    const NONE: Self = Self { raw: 0 };

    pub(crate) fn raw(self) -> u64 {
        self.raw
    }
}

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
    Status {
        code: Status,
        message: String,
    },
    InvalidString(NulError),
    Snapshot(SnapshotError),
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
            Self::Status { code, message } => write!(f, "{message} (status {code})"),
            Self::InvalidString(e) => write!(f, "string contains a NUL byte: {e}"),
            Self::Snapshot(e) => write!(f, "bad network snapshot: {e}"),
        }
    }
}

impl std::error::Error for NativeError {}

impl From<NulError> for NativeError {
    fn from(e: NulError) -> Self {
        Self::InvalidString(e)
    }
}

impl From<SnapshotError> for NativeError {
    fn from(e: SnapshotError) -> Self {
        Self::Snapshot(e)
    }
}

/// Function pointers resolved from the library, one per exported symbol used here.
#[derive(Clone, Copy)]
struct Api {
    status_message: extern "C" fn(Status) -> *const c_char,
    string_free: unsafe extern "C" fn(*mut c_char),
    world_create: unsafe extern "C" fn(*mut *mut RawWorld) -> Status,
    world_destroy: unsafe extern "C" fn(*mut RawWorld),
    network_root: unsafe extern "C" fn(*mut RawWorld, *mut NodeId) -> Status,
    create_node:
        unsafe extern "C" fn(*mut RawWorld, *const c_char, *const c_char, *mut NodeId) -> Status,
    create_link: unsafe extern "C" fn(*mut RawWorld, *const c_char, *mut LinkId) -> Status,
    add_internal_link: unsafe extern "C" fn(*mut RawWorld, NodeId, LinkId) -> Status,
    connect: unsafe extern "C" fn(*mut RawWorld, NodeId, NodeId, LinkId) -> Status,
    subscribe: unsafe extern "C" fn(*mut RawWorld, NodeId, LinkId) -> Status,
    snapshot_json: unsafe extern "C" fn(*mut RawWorld, *mut *mut c_char) -> Status,
}

/// The loaded native library and the function pointers resolved from it.
pub(crate) struct NativeLibrary {
    path: PathBuf,
    version: String,
    api: Api,
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
    /// Default location: next to the sandbox executable, where Cargo puts both builds (or one
    /// directory up, where test executables live). Overridden by the `EMERGENCE_LIB`
    /// environment variable.
    pub(crate) fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os(LIB_PATH_VAR) {
            return path.into();
        }
        let file_name = libloading::library_filename("emergence");
        let Ok(exe) = std::env::current_exe() else {
            return file_name.into();
        };
        let beside = exe.with_file_name(&file_name);
        match exe.parent().and_then(Path::parent) {
            Some(up) if !beside.exists() && up.join(&file_name).exists() => up.join(&file_name),
            _ => beside,
        }
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

            let api = Api {
                status_message: *library
                    .get(b"emergence_status_message\0")
                    .map_err(load_err)?,
                string_free: *library.get(b"emergence_string_free\0").map_err(load_err)?,
                world_create: *library.get(b"emergence_world_create\0").map_err(load_err)?,
                world_destroy: *library
                    .get(b"emergence_world_destroy\0")
                    .map_err(load_err)?,
                network_root: *library.get(b"emergence_network_root\0").map_err(load_err)?,
                create_node: *library
                    .get(b"emergence_network_create_node\0")
                    .map_err(load_err)?,
                create_link: *library
                    .get(b"emergence_network_create_link\0")
                    .map_err(load_err)?,
                add_internal_link: *library
                    .get(b"emergence_network_add_internal_link\0")
                    .map_err(load_err)?,
                connect: *library
                    .get(b"emergence_network_connect\0")
                    .map_err(load_err)?,
                subscribe: *library
                    .get(b"emergence_network_subscribe\0")
                    .map_err(load_err)?,
                snapshot_json: *library
                    .get(b"emergence_network_snapshot_json\0")
                    .map_err(load_err)?,
            };

            Ok(Arc::new(Self {
                path: path.to_path_buf(),
                version,
                api,
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

    fn check(&self, code: Status) -> Result<(), NativeError> {
        if code == STATUS_OK {
            return Ok(());
        }
        // Returns a static NUL-terminated string for any input.
        let message = (self.api.status_message)(code);
        // SAFETY: see above.
        let message = unsafe { CStr::from_ptr(message) };
        Err(NativeError::Status {
            code,
            message: message.to_string_lossy().into_owned(),
        })
    }
}

/// A world living inside the native library. Destroyed when dropped.
#[derive(Debug)]
pub(crate) struct NativeWorld {
    library: Arc<NativeLibrary>,
    handle: NonNull<RawWorld>,
}

// SAFETY for every `unsafe` block in this impl: `handle` is live until drop, `&mut self`
// guarantees it is not in use elsewhere, and every other pointer is a valid local or a
// NUL-terminated `CString` that outlives the call.
impl NativeWorld {
    pub(crate) fn new(library: Arc<NativeLibrary>) -> Result<Self, NativeError> {
        let mut raw = ptr::null_mut();
        library.check(unsafe { (library.api.world_create)(&raw mut raw) })?;
        let handle = NonNull::new(raw).ok_or_else(|| NativeError::Status {
            code: STATUS_OK,
            message: "library returned a null world".into(),
        })?;
        Ok(Self { library, handle })
    }

    fn api(&self) -> Api {
        self.library.api
    }

    fn check(&self, code: Status) -> Result<(), NativeError> {
        self.library.check(code)
    }

    pub(crate) fn root(&mut self) -> Result<NodeId, NativeError> {
        let mut id = NodeId { raw: 0 };
        self.check(unsafe { (self.api().network_root)(self.handle.as_ptr(), &raw mut id) })?;
        Ok(id)
    }

    pub(crate) fn create_node(&mut self, name: &str, kind: &str) -> Result<NodeId, NativeError> {
        let (name, kind) = (CString::new(name)?, CString::new(kind)?);
        let mut id = NodeId { raw: 0 };
        self.check(unsafe {
            (self.api().create_node)(
                self.handle.as_ptr(),
                name.as_ptr(),
                kind.as_ptr(),
                &raw mut id,
            )
        })?;
        Ok(id)
    }

    pub(crate) fn create_link(&mut self, name: &str) -> Result<LinkId, NativeError> {
        let name = CString::new(name)?;
        let mut id = LinkId::NONE;
        self.check(unsafe {
            (self.api().create_link)(self.handle.as_ptr(), name.as_ptr(), &raw mut id)
        })?;
        Ok(id)
    }

    pub(crate) fn add_internal_link(
        &mut self,
        owner: NodeId,
        link: LinkId,
    ) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().add_internal_link)(self.handle.as_ptr(), owner, link) })
    }

    pub(crate) fn connect(
        &mut self,
        parent: NodeId,
        node: NodeId,
        link: Option<LinkId>,
    ) -> Result<(), NativeError> {
        let link = link.unwrap_or(LinkId::NONE);
        self.check(unsafe { (self.api().connect)(self.handle.as_ptr(), parent, node, link) })
    }

    pub(crate) fn subscribe(&mut self, node: NodeId, link: LinkId) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().subscribe)(self.handle.as_ptr(), node, link) })
    }

    pub(crate) fn snapshot(&mut self) -> Result<Snapshot, NativeError> {
        let mut json = ptr::null_mut();
        self.check(unsafe { (self.api().snapshot_json)(self.handle.as_ptr(), &raw mut json) })?;
        // On success the library wrote an owned NUL-terminated string; free it right after.
        let parsed = unsafe {
            let parsed = Snapshot::from_json(CStr::from_ptr(json).to_bytes());
            (self.api().string_free)(json);
            parsed
        };
        Ok(parsed?)
    }
}

impl Drop for NativeWorld {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `world_create` and is destroyed exactly once, here.
        unsafe { (self.api().world_destroy)(self.handle.as_ptr()) };
    }
}
