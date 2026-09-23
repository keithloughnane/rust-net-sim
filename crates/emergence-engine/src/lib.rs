//! # Emergence Engine
//!
//! A hierarchical packet-routing network simulation: nodes nest inside other nodes, talk over
//! links, and route packets along explicit multi-hop paths.
//!
//! - [`Network`] holds the topology: [`ControlNode`]s and the [`Link`]s between them.
//! - [`ControllerLogic`] is the behaviour attached to a node. A few built-in kinds ship with the
//!   engine ([`LOGIC_KINDS`]).
//! - [`World`] ties them together and runs the simulation one [`World::tick`] at a time,
//!   recording what happened as [`TraceEvent`]s.
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

mod builtin;
mod ids;
mod logic;
mod monitor;
mod network;
mod packet;
mod route;
mod trace;
mod world;

pub use builtin::{
    Beacon, Bridge, FAULTY_LOGIC_KINDS, Gateway, LOGIC_KINDS, Responder, Scanner, create_logic,
    events as builtin_events, faulty,
};
pub use ids::{LinkId, NodeId};
pub use logic::{AcceptRule, Context, ControllerLogic, Relaying, SendError};
pub use monitor::{Alert, Check, MonitorConfig, Severity, Subject, Threshold};
pub use network::{ControlNode, Link, Network, NetworkError};
pub use packet::{Event, Packet, PacketId};
pub use route::{Hop, PacketRoute, RouteError, names};
pub use trace::{DropReason, TraceEvent};
pub use world::{FuseLimit, FuseReport, Health, Limits, LogicError, NodeStats, TickOutcome, World};

/// The version of this crate, as set in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
