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
use crate::trace::TraceEntry;

/// ABI version this module was written against (`EMERGENCE_ABI_VERSION` in the header).
const EXPECTED_ABI_VERSION: u32 = 5;

/// Environment variable that overrides where the library is loaded from.
const LIB_PATH_VAR: &str = "EMERGENCE_LIB";

/// `EmergenceStatus` from the header. Kept as a plain integer: a newer library may return codes
/// this module does not know.
type Status = u32;
const STATUS_OK: Status = 0;
const STATUS_FUSE_TRIPPED: Status = 18;

/// How a tick ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickResult {
    Completed,
    /// The world hit a hard limit and held packets back. The host should pause.
    FuseTripped,
}

/// The template catalogue (see `emergence_templates_catalog_json`).
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TemplateCatalog {
    pub(crate) templates: Vec<TemplateInfo>,
    pub(crate) apps: Vec<AppInfo>,
    pub(crate) hardware: Vec<String>,
    pub(crate) npc_roles: Vec<String>,
    pub(crate) defaults: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TemplateInfo {
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AppInfo {
    pub(crate) name: String,
    pub(crate) title: String,
}

/// A built-in logic kind offered by the library.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LogicKind {
    pub(crate) name: String,
    /// Deliberately broken, for stress testing.
    pub(crate) faulty: bool,
}

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
    Trace(serde_json::Error),
    /// A problem found by sandbox code rather than the library.
    Sandbox(String),
}

impl NativeError {
    /// The library's status code, if this came from a native call.
    pub(crate) fn status(&self) -> Option<Status> {
        match self {
            Self::Status { code, .. } => Some(*code),
            _ => None,
        }
    }
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
            Self::Trace(e) => write!(f, "bad trace: {e}"),
            Self::Sandbox(e) => f.write_str(e),
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
    world_tick: unsafe extern "C" fn(*mut RawWorld) -> Status,
    set_logic: unsafe extern "C" fn(*mut RawWorld, NodeId, *const c_char) -> Status,
    #[allow(clippy::type_complexity)] // Mirrors the C declaration one to one.
    send: unsafe extern "C" fn(
        *mut RawWorld,
        NodeId,
        *const c_char,
        *const c_char,
        *const c_char,
        *const u8,
        usize,
    ) -> Status,
    drain_trace_json: unsafe extern "C" fn(*mut RawWorld, *mut *mut c_char) -> Status,
    unsubscribe: unsafe extern "C" fn(*mut RawWorld, NodeId, LinkId) -> Status,
    disconnect: unsafe extern "C" fn(*mut RawWorld, NodeId, NodeId) -> Status,
    set_limits: unsafe extern "C" fn(*mut RawWorld, u64, u64, u64, u64) -> Status,
    set_trace_packets: unsafe extern "C" fn(*mut RawWorld, u32) -> Status,
    health_json: unsafe extern "C" fn(*mut RawWorld, *mut *mut c_char) -> Status,
    fuse_report_json: unsafe extern "C" fn(*mut RawWorld, *mut *mut c_char) -> Status,
    template_build: unsafe extern "C" fn(
        *mut RawWorld,
        *const c_char,
        *const c_char,
        *const c_char,
        *mut NodeId,
    ) -> Status,
    last_error: unsafe extern "C" fn(*const RawWorld) -> *const c_char,
    #[allow(clippy::type_complexity)] // Mirrors the C declaration one to one.
    template_build_at: unsafe extern "C" fn(
        *mut RawWorld,
        *const c_char,
        *const c_char,
        *const c_char,
        NodeId,
        LinkId,
        *mut NodeId,
    ) -> Status,
    remove_node: unsafe extern "C" fn(*mut RawWorld, NodeId) -> Status,
    remove_link: unsafe extern "C" fn(*mut RawWorld, LinkId) -> Status,
}

/// The loaded native library and the function pointers resolved from it.
pub(crate) struct NativeLibrary {
    path: PathBuf,
    version: String,
    logic_kinds: Vec<LogicKind>,
    templates: TemplateCatalog,
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
    #[allow(clippy::too_many_lines)] // One entry per exported symbol.
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
                world_tick: *library.get(b"emergence_world_tick\0").map_err(load_err)?,
                set_logic: *library
                    .get(b"emergence_world_set_logic\0")
                    .map_err(load_err)?,
                send: *library.get(b"emergence_world_send\0").map_err(load_err)?,
                drain_trace_json: *library
                    .get(b"emergence_world_drain_trace_json\0")
                    .map_err(load_err)?,
                unsubscribe: *library
                    .get(b"emergence_network_unsubscribe\0")
                    .map_err(load_err)?,
                disconnect: *library
                    .get(b"emergence_network_disconnect\0")
                    .map_err(load_err)?,
                set_limits: *library
                    .get(b"emergence_world_set_limits\0")
                    .map_err(load_err)?,
                set_trace_packets: *library
                    .get(b"emergence_world_set_trace_packets\0")
                    .map_err(load_err)?,
                health_json: *library
                    .get(b"emergence_world_health_json\0")
                    .map_err(load_err)?,
                fuse_report_json: *library
                    .get(b"emergence_world_fuse_report_json\0")
                    .map_err(load_err)?,
                template_build: *library
                    .get(b"emergence_template_build\0")
                    .map_err(load_err)?,
                last_error: *library
                    .get(b"emergence_world_last_error\0")
                    .map_err(load_err)?,
                template_build_at: *library
                    .get(b"emergence_template_build_at\0")
                    .map_err(load_err)?,
                remove_node: *library
                    .get(b"emergence_network_remove_node\0")
                    .map_err(load_err)?,
                remove_link: *library
                    .get(b"emergence_network_remove_link\0")
                    .map_err(load_err)?,
            };
            let catalog = *library
                .get::<extern "C" fn() -> *const c_char>(b"emergence_templates_catalog_json\0")
                .map_err(load_err)?;
            let templates: TemplateCatalog =
                serde_json::from_slice(CStr::from_ptr(catalog()).to_bytes())
                    .map_err(NativeError::Trace)?;
            let logic_kinds = *library
                .get::<extern "C" fn() -> *const c_char>(b"emergence_logic_kinds_json\0")
                .map_err(load_err)?;
            let logic_kinds: Vec<LogicKind> =
                serde_json::from_slice(CStr::from_ptr(logic_kinds()).to_bytes())
                    .map_err(NativeError::Trace)?;

            Ok(Arc::new(Self {
                path: path.to_path_buf(),
                version,
                logic_kinds,
                templates,
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

    /// The templates the library can build.
    pub(crate) fn templates(&self) -> &TemplateCatalog {
        &self.templates
    }

    /// The built-in logic kinds the library offers.
    pub(crate) fn logic_kinds(&self) -> &[LogicKind] {
        &self.logic_kinds
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

    /// The library this world lives in, for creating sibling worlds.
    pub(crate) fn library(&self) -> Arc<NativeLibrary> {
        Arc::clone(&self.library)
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

    pub(crate) fn tick(&mut self) -> Result<TickResult, NativeError> {
        let status = unsafe { (self.api().world_tick)(self.handle.as_ptr()) };
        if status == STATUS_FUSE_TRIPPED {
            return Ok(TickResult::FuseTripped);
        }
        self.check(status).map(|()| TickResult::Completed)
    }

    /// Builds a detached node from a template (step one of two: connecting is separate, and if
    /// it fails the node stays built). Errors carry the library's explanation.
    pub(crate) fn build_template(
        &mut self,
        template: &str,
        name: &str,
        spec_json: &str,
    ) -> Result<NodeId, NativeError> {
        self.build(template, name, spec_json, None)
    }

    /// Builds a node from a template and connects it in one step. If it fails, nothing was
    /// added.
    pub(crate) fn build_template_at(
        &mut self,
        template: &str,
        name: &str,
        spec_json: &str,
        parent: NodeId,
        link: Option<LinkId>,
    ) -> Result<NodeId, NativeError> {
        self.build(
            template,
            name,
            spec_json,
            Some((parent, link.unwrap_or(LinkId::NONE))),
        )
    }

    fn build(
        &mut self,
        template: &str,
        name: &str,
        spec_json: &str,
        at: Option<(NodeId, LinkId)>,
    ) -> Result<NodeId, NativeError> {
        let (t, n, s) = (
            CString::new(template)?,
            CString::new(name)?,
            CString::new(spec_json)?,
        );
        let mut id = NodeId { raw: 0 };
        let (w, api) = (self.handle.as_ptr(), self.api());
        let status = unsafe {
            match at {
                Some((parent, link)) => (api.template_build_at)(
                    w,
                    t.as_ptr(),
                    n.as_ptr(),
                    s.as_ptr(),
                    parent,
                    link,
                    &raw mut id,
                ),
                None => (api.template_build)(w, t.as_ptr(), n.as_ptr(), s.as_ptr(), &raw mut id),
            }
        };
        if status == STATUS_OK {
            return Ok(id);
        }
        // The world keeps a detailed message for template failures.
        let why = unsafe { CStr::from_ptr((api.last_error)(w)) };
        let why = why.to_string_lossy();
        match self.check(status) {
            // The detailed message already says what kind of error it is.
            Err(NativeError::Status { code, .. }) if !why.is_empty() => Err(NativeError::Status {
                code,
                message: why.into_owned(),
            }),
            other => other.map(|()| id),
        }
    }

    /// Deletes a node, everything inside it, and the links they own.
    pub(crate) fn remove_node(&mut self, node: NodeId) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().remove_node)(self.handle.as_ptr(), node) })
    }

    /// Deletes a link; its subscribers and owner stay.
    pub(crate) fn remove_link(&mut self, link: LinkId) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().remove_link)(self.handle.as_ptr(), link) })
    }

    pub(crate) fn unsubscribe(&mut self, node: NodeId, link: LinkId) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().unsubscribe)(self.handle.as_ptr(), node, link) })
    }

    pub(crate) fn disconnect(&mut self, parent: NodeId, node: NodeId) -> Result<(), NativeError> {
        self.check(unsafe { (self.api().disconnect)(self.handle.as_ptr(), parent, node) })
    }

    /// Sets the fuse's limits. 0 keeps a limit's current value.
    pub(crate) fn set_limits(
        &mut self,
        transmissions: u64,
        deliveries: u64,
        pending: u64,
        payload: u64,
    ) -> Result<(), NativeError> {
        self.check(unsafe {
            (self.api().set_limits)(
                self.handle.as_ptr(),
                transmissions,
                deliveries,
                pending,
                payload,
            )
        })
    }

    pub(crate) fn set_trace_packets(&mut self, enabled: bool) -> Result<(), NativeError> {
        self.check(unsafe {
            (self.api().set_trace_packets)(self.handle.as_ptr(), u32::from(enabled))
        })
    }

    pub(crate) fn health(&mut self) -> Result<crate::health::Health, NativeError> {
        let json = self.take_string(self.api().health_json)?;
        serde_json::from_slice(&json).map_err(NativeError::Trace)
    }

    /// Why the fuse tripped on the last tick, if it did.
    pub(crate) fn fuse_report(&mut self) -> Result<Option<crate::health::FuseReport>, NativeError> {
        let json = self.take_string(self.api().fuse_report_json)?;
        serde_json::from_slice(&json).map_err(NativeError::Trace)
    }

    pub(crate) fn set_logic(&mut self, node: NodeId, kind: &str) -> Result<(), NativeError> {
        let kind = CString::new(kind)?;
        self.check(unsafe { (self.api().set_logic)(self.handle.as_ptr(), node, kind.as_ptr()) })
    }

    pub(crate) fn send(
        &mut self,
        node: NodeId,
        via_link: &str,
        to_route: &str,
        event_kind: &str,
        data: &[u8],
    ) -> Result<(), NativeError> {
        let (via, to, kind) = (
            CString::new(via_link)?,
            CString::new(to_route)?,
            CString::new(event_kind)?,
        );
        self.check(unsafe {
            (self.api().send)(
                self.handle.as_ptr(),
                node,
                via.as_ptr(),
                to.as_ptr(),
                kind.as_ptr(),
                data.as_ptr(),
                data.len(),
            )
        })
    }

    /// Everything that happened since the last call.
    pub(crate) fn drain_trace(&mut self) -> Result<Vec<TraceEntry>, NativeError> {
        let json = self.take_string(self.api().drain_trace_json)?;
        crate::trace::parse(&json).map_err(NativeError::Trace)
    }

    /// Calls a `char **`-returning function and takes ownership of the string it returns.
    fn take_string(
        &mut self,
        f: unsafe extern "C" fn(*mut RawWorld, *mut *mut c_char) -> Status,
    ) -> Result<Vec<u8>, NativeError> {
        let mut out = ptr::null_mut();
        self.check(unsafe { f(self.handle.as_ptr(), &raw mut out) })?;
        // On success the library wrote an owned NUL-terminated string; free it right after.
        let bytes = unsafe {
            let bytes = CStr::from_ptr(out).to_bytes().to_vec();
            (self.api().string_free)(out);
            bytes
        };
        Ok(bytes)
    }

    pub(crate) fn snapshot(&mut self) -> Result<Snapshot, NativeError> {
        let json = self.take_string(self.api().snapshot_json)?;
        Ok(Snapshot::from_json(&json)?)
    }
}

// SAFETY: a world handle has no thread affinity (the engine's `World` is `Send`), and
// `NativeWorld` owns its handle exclusively, so moving it to another thread is sound. It is not
// `Sync`: calls still need `&mut self`.
unsafe impl Send for NativeWorld {}

impl Drop for NativeWorld {
    fn drop(&mut self) {
        // SAFETY: `handle` came from `world_create` and is destroyed exactly once, here.
        unsafe { (self.api().world_destroy)(self.handle.as_ptr()) };
    }
}
