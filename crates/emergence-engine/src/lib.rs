//! # Emergence Engine
//!
//! A hierarchical packet-routing network simulation: nodes nest inside other nodes, talk over
//! links, and route packets along explicit multi-hop paths.
//!
//! The crate has no UI or game-engine dependencies. Native hosts (Unity, Unreal) use it through
//! the C ABI in the `emergence-ffi` crate.
//!
//! ```
//! use emergence_engine::World;
//!
//! let mut world = World::new();
//! world.tick();
//! assert_eq!(world.tick_count(), 1);
//! ```

#![forbid(unsafe_code)]

mod ids;
mod network;
mod world;

pub use ids::{LinkId, NodeId};
pub use network::{ControlNode, Link, Network, NetworkError};
pub use world::World;

/// The version of this crate, as set in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
