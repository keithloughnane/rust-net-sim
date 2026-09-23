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
//! - Fallible functions return [`EmergenceStatus`] and write results through out-pointers.
//! - Panics never unwind into the host. They are caught and reported as
//!   [`EmergenceStatus::Panic`].
//! - Hosts should check [`emergence_abi_version`] against [`EMERGENCE_ABI_VERSION`] from the
//!   header they were built with before calling anything else.

#![allow(unsafe_code)] // Exporting a C ABI is unsafe by nature; keep it confined to this crate.

use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

use emergence_engine::World;

/// Version of the C ABI. Bump whenever an exported signature or type layout changes.
pub const EMERGENCE_ABI_VERSION: u32 = 1;

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
}

/// Opaque handle to a simulation world.
#[derive(Debug)]
pub struct EmergenceWorld(World);

/// Runs `f`, converting a panic into [`EmergenceStatus::Panic`] so it never reaches the host.
fn guard(f: impl FnOnce() -> EmergenceStatus) -> EmergenceStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(EmergenceStatus::Panic)
}

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
    // SAFETY: the caller guarantees the handle is null or live and not aliased.
    let Some(world) = (unsafe { world.as_mut() }) else {
        return EmergenceStatus::NullPointer;
    };
    guard(|| {
        world.0.tick();
        EmergenceStatus::Ok
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::ptr;

    #[test]
    fn version_is_nul_terminated_crate_version() {
        // SAFETY: the returned pointer is a static NUL-terminated string.
        let version = unsafe { CStr::from_ptr(emergence_version()) };
        assert_eq!(version.to_str(), Ok(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn world_lifecycle_through_the_abi() {
        let mut world = ptr::null_mut();
        let mut count = u64::MAX;
        // SAFETY: all pointers are valid locals, and the handle is used only until destroyed.
        unsafe {
            assert_eq!(emergence_world_create(&raw mut world), EmergenceStatus::Ok);
            assert_eq!(emergence_world_tick(world), EmergenceStatus::Ok);
            assert_eq!(
                emergence_world_tick_count(world, &raw mut count),
                EmergenceStatus::Ok
            );
            emergence_world_destroy(world);
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn null_pointers_are_rejected_not_dereferenced() {
        let mut count = 0;
        // SAFETY: null is an allowed input for every one of these functions.
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
            emergence_world_destroy(ptr::null_mut());
        }
    }
}
