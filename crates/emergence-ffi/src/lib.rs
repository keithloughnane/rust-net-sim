//! C ABI for the Emergence Engine, for Unity (C#), Unreal (C++) and any other native host.
//!
//! The generated headers and bindings live in `bindings/` and are regenerated with
//! `cargo xtask bindings`.
//!
//! # Conventions
//!
//! - Every exported symbol is prefixed `emergence_`.
//! - Objects are opaque handles. The library creates and destroys them; hosts never free them
//!   directly and never look inside them.
//! - Nodes and links are identified by small ID structs. A `raw` value of 0 means "none".
//! - Strings passed in are NUL-terminated UTF-8, borrowed only for the duration of the call.
//! - Strings returned as `const char *` are static. Strings returned through a `char **`
//!   out-parameter are owned by the caller and must be freed with [`emergence_string_free`].
//! - Fallible functions return [`EmergenceStatus`] and write results through out-pointers. On
//!   failure nothing is written and nothing changes.
//! - Panics never unwind into the host. They are caught and reported as
//!   [`EmergenceStatus::Panic`].
//! - Hosts should check [`emergence_abi_version`] against [`EMERGENCE_ABI_VERSION`] from the
//!   header they were built with before calling anything else.

#![allow(unsafe_code)] // Exporting a C ABI is unsafe by nature; keep it confined to this crate.

mod snapshot;
mod trace;

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use emergence_engine::{
    Event, LOGIC_KINDS, LinkId, LogicError, NetworkError, NodeId, PacketRoute, SendError, World,
};

/// Version of the C ABI. Bump whenever an exported signature or type layout changes.
pub const EMERGENCE_ABI_VERSION: u32 = 3;

static VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Result of a fallible call. Always 32 bits wide, whatever the host compiler does with enums.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergenceStatus {
    /// The call succeeded.
    Ok = 0,
    /// A required pointer argument was null.
    NullPointer = 1,
    /// The library hit an internal error. The object involved should be destroyed.
    Panic = 2,
    /// A string argument was not valid UTF-8.
    InvalidString = 3,
    /// A node ID does not refer to a node in this world.
    UnknownNode = 4,
    /// A link ID does not refer to a link in this world.
    UnknownLink = 5,
    /// The node already has a different parent. Disconnect it first.
    AlreadyHasParent = 6,
    /// The operation would nest a node inside itself.
    WouldCreateCycle = 7,
    /// The root node cannot be given a parent.
    IsRoot = 8,
    /// The link is already internal to a different node.
    LinkOwnedElsewhere = 9,
    /// The node is not a child of the given parent.
    NotAChild = 10,
    /// No built-in logic has that name.
    UnknownLogic = 11,
    /// A route string could not be parsed, or a route would be too deep.
    InvalidRoute = 12,
    /// The node is neither subscribed to the link nor its owner, so it cannot send on it.
    NotOnLink = 13,
    /// The operation failed for a reason this ABI version does not have a code for.
    Failed = 255,
}

impl EmergenceStatus {
    const ALL: [Self; 15] = [
        Self::Ok,
        Self::NullPointer,
        Self::Panic,
        Self::InvalidString,
        Self::UnknownNode,
        Self::UnknownLink,
        Self::AlreadyHasParent,
        Self::WouldCreateCycle,
        Self::IsRoot,
        Self::LinkOwnedElsewhere,
        Self::NotAChild,
        Self::UnknownLogic,
        Self::InvalidRoute,
        Self::NotOnLink,
        Self::Failed,
    ];

    fn message(self) -> &'static CStr {
        match self {
            Self::Ok => c"ok",
            Self::NullPointer => c"a required pointer argument was null",
            Self::Panic => c"the library hit an internal error",
            Self::InvalidString => c"a string argument was not valid UTF-8",
            Self::UnknownNode => c"unknown node",
            Self::UnknownLink => c"unknown link",
            Self::AlreadyHasParent => c"the node already has a different parent",
            Self::WouldCreateCycle => c"a node cannot be nested inside itself",
            Self::IsRoot => c"the root node cannot have a parent",
            Self::LinkOwnedElsewhere => c"the link is already internal to another node",
            Self::NotAChild => c"the node is not a child of that parent",
            Self::UnknownLogic => c"no built-in logic has that name",
            Self::InvalidRoute => c"invalid route",
            Self::NotOnLink => c"the node is not on that link",
            Self::Failed => c"the operation failed",
        }
    }
}

impl From<NetworkError> for EmergenceStatus {
    fn from(error: NetworkError) -> Self {
        match error {
            NetworkError::UnknownNode(_) => Self::UnknownNode,
            NetworkError::UnknownLink(_) => Self::UnknownLink,
            NetworkError::AlreadyHasParent(_) => Self::AlreadyHasParent,
            NetworkError::WouldCreateCycle => Self::WouldCreateCycle,
            NetworkError::IsRoot => Self::IsRoot,
            NetworkError::LinkOwnedElsewhere(_) => Self::LinkOwnedElsewhere,
            NetworkError::NotAChild(_) => Self::NotAChild,
            _ => Self::Failed,
        }
    }
}

impl From<LogicError> for EmergenceStatus {
    fn from(error: LogicError) -> Self {
        match error {
            LogicError::UnknownNode(_) => Self::UnknownNode,
            LogicError::UnknownKind(_) => Self::UnknownLogic,
            _ => Self::Failed,
        }
    }
}

impl From<SendError> for EmergenceStatus {
    fn from(error: SendError) -> Self {
        match error {
            SendError::UnknownNode(_) => Self::UnknownNode,
            SendError::UnknownLink(_) => Self::UnknownLink,
            SendError::NotOnLink(_) => Self::NotOnLink,
            SendError::Route(_) => Self::InvalidRoute,
            _ => Self::Failed,
        }
    }
}

impl<E: Into<Self>> From<Result<(), E>> for EmergenceStatus {
    fn from(result: Result<(), E>) -> Self {
        result.map_or_else(Into::into, |()| Self::Ok)
    }
}

/// Opaque handle to a simulation world.
#[derive(Debug)]
pub struct EmergenceWorld(World);

/// Identifies a node within a world. `raw == 0` means "no node".
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergenceNodeId {
    /// Opaque value. Only compare it or pass it back; do not do arithmetic on it.
    pub raw: u64,
}

/// Identifies a link within a world. `raw == 0` means "no link".
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergenceLinkId {
    /// Opaque value. Only compare it or pass it back; do not do arithmetic on it.
    pub raw: u64,
}

impl From<NodeId> for EmergenceNodeId {
    fn from(id: NodeId) -> Self {
        Self { raw: id.to_raw() }
    }
}

impl From<EmergenceNodeId> for NodeId {
    fn from(id: EmergenceNodeId) -> Self {
        Self::from_raw(id.raw)
    }
}

impl From<LinkId> for EmergenceLinkId {
    fn from(id: LinkId) -> Self {
        Self { raw: id.to_raw() }
    }
}

impl From<EmergenceLinkId> for LinkId {
    fn from(id: EmergenceLinkId) -> Self {
        Self::from_raw(id.raw)
    }
}

/// Runs `f`, converting a panic into [`EmergenceStatus::Panic`] so it never reaches the host.
fn guard(f: impl FnOnce() -> EmergenceStatus) -> EmergenceStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(EmergenceStatus::Panic)
}

/// Resolves a world handle and runs `f` on it inside [`guard`].
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
unsafe fn with_world(
    world: *mut EmergenceWorld,
    f: impl FnOnce(&mut World) -> EmergenceStatus,
) -> EmergenceStatus {
    // SAFETY: the caller guarantees the handle is null or live and not aliased.
    match unsafe { world.as_mut() } {
        Some(world) => guard(|| f(&mut world.0)),
        None => EmergenceStatus::NullPointer,
    }
}

/// Borrows a NUL-terminated UTF-8 string from the host.
///
/// # Safety
///
/// `ptr` must be null or point to a NUL-terminated string that outlives `'a`.
unsafe fn borrow_str<'a>(ptr: *const c_char) -> Result<&'a str, EmergenceStatus> {
    if ptr.is_null() {
        return Err(EmergenceStatus::NullPointer);
    }
    // SAFETY: non-null, and the caller guarantees NUL termination and lifetime.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| EmergenceStatus::InvalidString)
}

// ---------------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------------

/// Returns the ABI version this library was built with.
#[unsafe(no_mangle)]
pub extern "C" fn emergence_abi_version() -> u32 {
    EMERGENCE_ABI_VERSION
}

/// Returns the library version as a static, NUL-terminated UTF-8 string. Do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn emergence_version() -> *const c_char {
    VERSION.as_ptr().cast()
}

/// Returns a short English description of an [`EmergenceStatus`] value as a static,
/// NUL-terminated string. Do not free it.
///
/// Takes a plain integer so that any value, including codes from a newer library, is safe to
/// pass.
#[unsafe(no_mangle)]
pub extern "C" fn emergence_status_message(status: u32) -> *const c_char {
    EmergenceStatus::ALL
        .into_iter()
        .find(|s| *s as u32 == status)
        .map_or(c"unknown status", EmergenceStatus::message)
        .as_ptr()
}

/// Frees a string returned through a `char **` out-parameter. Passing null is a no-op.
///
/// # Safety
///
/// `string` must be null or a string this library returned that has not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_string_free(string: *mut c_char) {
    if !string.is_null() {
        // SAFETY: the caller guarantees this came from `CString::into_raw` below.
        drop(unsafe { CString::from_raw(string) });
    }
}

// ---------------------------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------------------------

/// Creates a new world and writes its handle to `out_world`.
///
/// Destroy it with [`emergence_world_destroy`].
///
/// # Safety
///
/// `out_world` must be null or valid for a pointer-sized write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_create(
    out_world: *mut *mut EmergenceWorld,
) -> EmergenceStatus {
    if out_world.is_null() {
        return EmergenceStatus::NullPointer;
    }
    guard(|| {
        let world = Box::into_raw(Box::new(EmergenceWorld(World::new())));
        // SAFETY: checked non-null above; the caller guarantees it is writable.
        unsafe { out_world.write(world) };
        EmergenceStatus::Ok
    })
}

/// Destroys a world. Passing null is a no-op.
///
/// # Safety
///
/// `world` must be null or a handle from [`emergence_world_create`] that has not already been
/// destroyed. The handle must not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_destroy(world: *mut EmergenceWorld) {
    if !world.is_null() {
        // SAFETY: the caller guarantees this is a live handle we allocated with `Box`.
        drop(unsafe { Box::from_raw(world) });
    }
}

/// Advances the world by one tick.
///
/// # Safety
///
/// `world` must be null or a live handle from [`emergence_world_create`], not in use on
/// another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_tick(world: *mut EmergenceWorld) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world.tick();
            EmergenceStatus::Ok
        })
    }
}

/// Writes the number of ticks the world has run to `out_count`.
///
/// # Safety
///
/// `world` must be null or a live handle from [`emergence_world_create`]. `out_count` must be
/// null or valid for a `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_tick_count(
    world: *const EmergenceWorld,
    out_count: *mut u64,
) -> EmergenceStatus {
    // SAFETY: the caller guarantees the handle is null or live.
    let Some(world) = (unsafe { world.as_ref() }) else {
        return EmergenceStatus::NullPointer;
    };
    if out_count.is_null() {
        return EmergenceStatus::NullPointer;
    }
    guard(|| {
        // SAFETY: checked non-null above; the caller guarantees it is writable.
        unsafe { out_count.write(world.0.tick_count()) };
        EmergenceStatus::Ok
    })
}

// ---------------------------------------------------------------------------------------------
// Network topology
// ---------------------------------------------------------------------------------------------

/// Writes the ID of the world's root node to `out_node`.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `out_node` must be null
/// or valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_root(
    world: *mut EmergenceWorld,
    out_node: *mut EmergenceNodeId,
) -> EmergenceStatus {
    if out_node.is_null() {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            out_node.write(world.network().root().into());
            EmergenceStatus::Ok
        })
    }
}

/// Creates a detached node and writes its ID to `out_node`. Attach it with
/// [`emergence_network_connect`].
///
/// `kind` is a free-form label such as `"computer"` or `"app"`.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `name` and `kind` must
/// be null or NUL-terminated strings. `out_node` must be null or valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_create_node(
    world: *mut EmergenceWorld,
    name: *const c_char,
    kind: *const c_char,
    out_node: *mut EmergenceNodeId,
) -> EmergenceStatus {
    if out_node.is_null() {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            let (name, kind) = match (borrow_str(name), borrow_str(kind)) {
                (Ok(name), Ok(kind)) => (name, kind),
                (Err(status), _) | (_, Err(status)) => return status,
            };
            let id = world.network_mut().create_node(name, kind);
            out_node.write(id.into());
            EmergenceStatus::Ok
        })
    }
}

/// Creates a link with no owner and no subscribers, and writes its ID to `out_link`.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `name` must be null or
/// a NUL-terminated string. `out_link` must be null or valid for a write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_create_link(
    world: *mut EmergenceWorld,
    name: *const c_char,
    out_link: *mut EmergenceLinkId,
) -> EmergenceStatus {
    if out_link.is_null() {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            let name = match borrow_str(name) {
                Ok(name) => name,
                Err(status) => return status,
            };
            let id = world.network_mut().create_link(name);
            out_link.write(id.into());
            EmergenceStatus::Ok
        })
    }
}

/// Makes `link` internal to `owner`, so `owner`'s children can use it. Does nothing if it
/// already is.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_add_internal_link(
    world: *mut EmergenceWorld,
    owner: EmergenceNodeId,
    link: EmergenceLinkId,
) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world
                .network_mut()
                .add_internal_link(owner.into(), link.into())
                .into()
        })
    }
}

/// Nests `node` inside `parent`. If `link.raw` is not 0, also makes `link` internal to
/// `parent` and subscribes `node` to it.
///
/// Connecting a node to the parent it already has is allowed; that is how a child joins a
/// second internal link.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_connect(
    world: *mut EmergenceWorld,
    parent: EmergenceNodeId,
    node: EmergenceNodeId,
    link: EmergenceLinkId,
) -> EmergenceStatus {
    let link = (link.raw != 0).then(|| link.into());
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world
                .network_mut()
                .connect(parent.into(), node.into(), link)
                .into()
        })
    }
}

/// Attaches `node` to `link` without changing the hierarchy. Does nothing if it already is.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_subscribe(
    world: *mut EmergenceWorld,
    node: EmergenceNodeId,
    link: EmergenceLinkId,
) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world
                .network_mut()
                .subscribe(node.into(), link.into())
                .into()
        })
    }
}

/// Detaches `node` from `link`. Does nothing if it was not attached.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_unsubscribe(
    world: *mut EmergenceWorld,
    node: EmergenceNodeId,
    link: EmergenceLinkId,
) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world
                .network_mut()
                .unsubscribe(node.into(), link.into())
                .into()
        })
    }
}

/// Removes `node` from `parent`, the inverse of [`emergence_network_connect`]. The node also
/// leaves all of `parent`'s internal links but keeps its own children and other links.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_disconnect(
    world: *mut EmergenceWorld,
    parent: EmergenceNodeId,
    node: EmergenceNodeId,
) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            world
                .network_mut()
                .disconnect(parent.into(), node.into())
                .into()
        })
    }
}

/// Writes a JSON description of the whole network to `out_json`. Free it with
/// [`emergence_string_free`].
///
/// The format is documented in `crates/emergence-ffi/src/snapshot.rs` and carries its own
/// `"format"` version number.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `out_json` must be null
/// or valid for a pointer-sized write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_network_snapshot_json(
    world: *mut EmergenceWorld,
    out_json: *mut *mut c_char,
) -> EmergenceStatus {
    if out_json.is_null() {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            write_json(&snapshot::Snapshot::of(world), out_json)
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Logic and traffic
// ---------------------------------------------------------------------------------------------

/// Returns the names of the built-in logic kinds as a static JSON array of strings, such as
/// `["responder","gateway"]`. Do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn emergence_logic_kinds_json() -> *const c_char {
    static KINDS: OnceLock<CString> = OnceLock::new();
    KINDS
        .get_or_init(|| {
            let json = serde_json::to_string(LOGIC_KINDS).unwrap_or_else(|_| "[]".into());
            CString::new(json).unwrap_or_default()
        })
        .as_ptr()
}

/// Attaches a built-in logic to `node` by name (see [`emergence_logic_kinds_json`]). An empty
/// string or `"none"` removes the node's logic.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `kind` must be null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_set_logic(
    world: *mut EmergenceWorld,
    node: EmergenceNodeId,
    kind: *const c_char,
) -> EmergenceStatus {
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| match borrow_str(kind) {
            Ok(kind) => world.set_logic_kind(node.into(), kind).into(),
            Err(status) => status,
        })
    }
}

/// Queues an event from `node`, as if its own logic had sent it. It is delivered on the next
/// [`emergence_world_tick`].
///
/// - `via_link` names the link to transmit on: one `node` subscribes to or owns.
/// - `to_route` is the destination in route text form: `node@link`, or several hops joined by
///   `/` such as `pc-1@wifi/fileman@ipc`. `*` addresses everyone, `^` the parent.
/// - `data` may be null when `data_len` is 0.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. The strings must be
/// null or NUL-terminated. `data` must be null or valid for `data_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_send(
    world: *mut EmergenceWorld,
    node: EmergenceNodeId,
    via_link: *const c_char,
    to_route: *const c_char,
    event_kind: *const c_char,
    data: *const u8,
    data_len: usize,
) -> EmergenceStatus {
    if data.is_null() && data_len != 0 {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            let (via, to, kind) = match (
                borrow_str(via_link),
                borrow_str(to_route),
                borrow_str(event_kind),
            ) {
                (Ok(v), Ok(t), Ok(k)) => (v, t, k),
                (Err(s), _, _) | (_, Err(s), _) | (_, _, Err(s)) => return s,
            };
            let Ok(to) = to.parse::<PacketRoute>() else {
                return EmergenceStatus::InvalidRoute;
            };
            let node: NodeId = node.into();
            if world.network().node(node).is_none() {
                return EmergenceStatus::UnknownNode;
            }
            let Some(via) = world.network().usable_link_named(node, via) else {
                return EmergenceStatus::NotOnLink;
            };
            let bytes = if data_len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data, data_len).to_vec()
            };
            world
                .send(node, via, to, Event::with_data(kind, bytes))
                .into()
        })
    }
}

/// Writes a JSON description of everything that happened since the last call (packets sent,
/// delivered and dropped, and notes from logic) to `out_json`, and clears it. Free the string
/// with [`emergence_string_free`].
///
/// The format is documented in `crates/emergence-ffi/src/trace.rs` and carries its own
/// `"format"` version number. The library keeps a bounded buffer; drain it regularly.
///
/// # Safety
///
/// `world` must be null or a live handle, not in use on another thread. `out_json` must be null
/// or valid for a pointer-sized write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn emergence_world_drain_trace_json(
    world: *mut EmergenceWorld,
    out_json: *mut *mut c_char,
) -> EmergenceStatus {
    if out_json.is_null() {
        return EmergenceStatus::NullPointer;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe {
        with_world(world, |world| {
            let events = world.drain_trace();
            let trace = trace::Trace::of(events, world.trace_discarded());
            write_json(&trace, out_json)
        })
    }
}

/// Serializes `value` and hands ownership of the string to the caller through `out`.
///
/// # Safety
///
/// `out` must be valid for a pointer-sized write.
unsafe fn write_json(value: &impl serde::Serialize, out: *mut *mut c_char) -> EmergenceStatus {
    let Ok(json) = serde_json::to_string(value) else {
        return EmergenceStatus::Failed;
    };
    // JSON escapes control characters, so it can never contain an interior NUL.
    let Ok(json) = CString::new(json) else {
        return EmergenceStatus::Failed;
    };
    // SAFETY: the caller guarantees `out` is writable.
    unsafe { out.write(json.into_raw()) };
    EmergenceStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    /// Owns a world for the duration of a test.
    struct TestWorld(*mut EmergenceWorld);

    impl TestWorld {
        fn new() -> Self {
            let mut world = ptr::null_mut();
            // SAFETY: `world` is a valid out-pointer.
            assert_eq!(
                unsafe { emergence_world_create(&raw mut world) },
                EmergenceStatus::Ok
            );
            Self(world)
        }

        fn node(&self, name: &CStr) -> EmergenceNodeId {
            let mut id = EmergenceNodeId { raw: 0 };
            // SAFETY: live world, valid strings and out-pointer.
            let status = unsafe {
                emergence_network_create_node(self.0, name.as_ptr(), c"test".as_ptr(), &raw mut id)
            };
            assert_eq!(status, EmergenceStatus::Ok);
            id
        }

        fn link(&self, name: &CStr) -> EmergenceLinkId {
            let mut id = EmergenceLinkId { raw: 0 };
            // SAFETY: live world, valid string and out-pointer.
            let status =
                unsafe { emergence_network_create_link(self.0, name.as_ptr(), &raw mut id) };
            assert_eq!(status, EmergenceStatus::Ok);
            id
        }

        fn root(&self) -> EmergenceNodeId {
            let mut id = EmergenceNodeId { raw: 0 };
            // SAFETY: live world, valid out-pointer.
            assert_eq!(
                unsafe { emergence_network_root(self.0, &raw mut id) },
                EmergenceStatus::Ok
            );
            id
        }

        fn snapshot(&self) -> serde_json::Value {
            let mut json = ptr::null_mut();
            // SAFETY: live world, valid out-pointer; the string is freed below.
            unsafe {
                assert_eq!(
                    emergence_network_snapshot_json(self.0, &raw mut json),
                    EmergenceStatus::Ok
                );
                let value = serde_json::from_slice(CStr::from_ptr(json).to_bytes());
                emergence_string_free(json);
                value.unwrap_or_default()
            }
        }
    }

    impl Drop for TestWorld {
        fn drop(&mut self) {
            // SAFETY: created in `new`, destroyed exactly once.
            unsafe { emergence_world_destroy(self.0) };
        }
    }

    #[test]
    fn version_is_nul_terminated_crate_version() {
        // SAFETY: the returned pointer is a static NUL-terminated string.
        let version = unsafe { CStr::from_ptr(emergence_version()) };
        assert_eq!(version.to_str(), Ok(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn world_ticks_through_the_abi() {
        let world = TestWorld::new();
        let mut count = u64::MAX;
        // SAFETY: live world and valid out-pointer.
        unsafe {
            assert_eq!(emergence_world_tick(world.0), EmergenceStatus::Ok);
            assert_eq!(
                emergence_world_tick_count(world.0, &raw mut count),
                EmergenceStatus::Ok
            );
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn null_pointers_are_rejected_not_dereferenced() {
        let world = TestWorld::new();
        let mut count = 0;
        let mut node = EmergenceNodeId { raw: 0 };
        // SAFETY: null is an allowed input for every one of these arguments.
        unsafe {
            assert_eq!(
                emergence_world_create(ptr::null_mut()),
                EmergenceStatus::NullPointer
            );
            assert_eq!(
                emergence_world_tick(ptr::null_mut()),
                EmergenceStatus::NullPointer
            );
            assert_eq!(
                emergence_world_tick_count(ptr::null(), &raw mut count),
                EmergenceStatus::NullPointer
            );
            assert_eq!(
                emergence_network_create_node(world.0, ptr::null(), c"k".as_ptr(), &raw mut node),
                EmergenceStatus::NullPointer
            );
            emergence_world_destroy(ptr::null_mut());
            emergence_string_free(ptr::null_mut());
        }
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let world = TestWorld::new();
        let mut node = EmergenceNodeId { raw: 0 };
        let bad = [0xFF_u8, 0];
        // SAFETY: live world; `bad` is NUL-terminated.
        let status = unsafe {
            emergence_network_create_node(
                world.0,
                bad.as_ptr().cast(),
                c"k".as_ptr(),
                &raw mut node,
            )
        };
        assert_eq!(status, EmergenceStatus::InvalidString);
        assert_eq!(node.raw, 0);
    }

    #[test]
    fn build_network_and_snapshot_it() {
        let world = TestWorld::new();
        let root = world.root();
        let pc = world.node(c"pc");
        let app = world.node(c"app");
        let wifi = world.link(c"wifi");
        let ipc = world.link(c"ipc");
        // SAFETY: live world.
        unsafe {
            assert_eq!(
                emergence_network_connect(world.0, root, pc, wifi),
                EmergenceStatus::Ok
            );
            assert_eq!(
                emergence_network_connect(world.0, pc, app, ipc),
                EmergenceStatus::Ok
            );
            assert_eq!(
                emergence_network_connect(world.0, app, root, EmergenceLinkId { raw: 0 }),
                EmergenceStatus::IsRoot
            );
        }

        let snap = world.snapshot();
        assert_eq!(snap["format"], snapshot::FORMAT);
        assert_eq!(snap["root"], root.raw);
        assert_eq!(snap["nodes"].as_array().map(Vec::len), Some(3));
        let app_entry = &snap["nodes"][2];
        assert_eq!(app_entry["name"], "app");
        assert_eq!(app_entry["parent"], pc.raw);
        assert_eq!(app_entry["subscriptions"][0], ipc.raw);
        assert_eq!(snap["links"][1]["owner"], pc.raw);
    }

    #[test]
    fn every_status_has_a_message() {
        for status in EmergenceStatus::ALL {
            // SAFETY: returns a static NUL-terminated string.
            let message = unsafe { CStr::from_ptr(emergence_status_message(status as u32)) };
            assert_ne!(message, c"unknown status");
        }
        // SAFETY: returns a static NUL-terminated string.
        let unknown = unsafe { CStr::from_ptr(emergence_status_message(12345)) };
        assert_eq!(unknown, c"unknown status");
    }

    impl TestWorld {
        fn drain_trace(&self) -> serde_json::Value {
            let mut json = ptr::null_mut();
            // SAFETY: live world, valid out-pointer; the string is freed below.
            unsafe {
                assert_eq!(
                    emergence_world_drain_trace_json(self.0, &raw mut json),
                    EmergenceStatus::Ok
                );
                let value = serde_json::from_slice(CStr::from_ptr(json).to_bytes());
                emergence_string_free(json);
                value.unwrap_or_default()
            }
        }
    }

    #[test]
    fn ping_through_the_abi_gets_a_pong() {
        let world = TestWorld::new();
        let (root, a, b, wifi) = (
            world.root(),
            world.node(c"a"),
            world.node(c"b"),
            world.link(c"wifi"),
        );
        // SAFETY: live world, valid strings; data is null with length 0.
        unsafe {
            assert_eq!(
                emergence_network_connect(world.0, root, a, wifi),
                EmergenceStatus::Ok
            );
            assert_eq!(
                emergence_network_connect(world.0, root, b, wifi),
                EmergenceStatus::Ok
            );
            assert_eq!(
                emergence_world_set_logic(world.0, b, c"responder".as_ptr()),
                EmergenceStatus::Ok
            );
            let data = b"hello";
            assert_eq!(
                emergence_world_send(
                    world.0,
                    a,
                    c"wifi".as_ptr(),
                    c"b@wifi".as_ptr(),
                    c"ping".as_ptr(),
                    data.as_ptr(),
                    data.len()
                ),
                EmergenceStatus::Ok
            );
            emergence_world_tick(world.0);
            emergence_world_tick(world.0);
        }
        let trace = world.drain_trace();
        assert_eq!(trace["format"], trace::FORMAT);
        let events = trace["events"].as_array().cloned().unwrap_or_default();
        let kinds: Vec<(&str, &str)> = events
            .iter()
            .filter(|e| e["type"] == "sent")
            .map(|e| {
                (
                    e["kind"].as_str().unwrap_or(""),
                    e["data"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(kinds, vec![("ping", "hello"), ("pong", "hello")]);

        let snap = world.snapshot();
        assert_eq!(snap["tick"], 2);
        assert_eq!(snap["nodes"][2]["logic"], "responder");
        assert_eq!(snap["nodes"][1]["received"], 1);
        assert!(
            world.drain_trace()["events"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
    }

    #[test]
    fn send_and_logic_errors_have_their_own_codes() {
        let world = TestWorld::new();
        let (root, a, wifi) = (world.root(), world.node(c"a"), world.link(c"wifi"));
        // SAFETY: live world and valid strings.
        unsafe {
            assert_eq!(
                emergence_network_connect(world.0, root, a, wifi),
                EmergenceStatus::Ok
            );
            assert_eq!(
                emergence_world_set_logic(world.0, a, c"teleporter".as_ptr()),
                EmergenceStatus::UnknownLogic
            );
            let send = |via: &CStr, to: &CStr| {
                emergence_world_send(
                    world.0,
                    a,
                    via.as_ptr(),
                    to.as_ptr(),
                    c"x".as_ptr(),
                    ptr::null(),
                    0,
                )
            };
            assert_eq!(send(c"wifi", c"@@"), EmergenceStatus::InvalidRoute);
            assert_eq!(send(c"cable", c"b@cable"), EmergenceStatus::NotOnLink);
            assert_eq!(
                emergence_world_send(
                    world.0,
                    a,
                    c"wifi".as_ptr(),
                    c"b".as_ptr(),
                    c"x".as_ptr(),
                    ptr::null(),
                    3
                ),
                EmergenceStatus::NullPointer
            );
        }
    }

    #[test]
    fn logic_kinds_are_listed() {
        // SAFETY: returns a static NUL-terminated string.
        let json = unsafe { CStr::from_ptr(emergence_logic_kinds_json()) };
        let kinds: Vec<String> = serde_json::from_slice(json.to_bytes()).unwrap_or_default();
        assert!(kinds.iter().any(|k| k == "gateway"));
    }
}
