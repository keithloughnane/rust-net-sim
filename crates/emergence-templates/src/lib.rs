//! # Emergence Templates
//!
//! Pre-built composite nodes for the Emergence Engine: one builder function per kind of thing
//! (a computer, an NPC), each with its own parameters, each returning a complete, wired-up,
//! detached subtree for the caller to connect wherever it belongs.
//!
//! - **Encapsulated**: callers get the root node and use it like any other node. What is inside
//!   can grow without breaking them.
//! - **A fixed catalogue**: the apps a computer can have ([`AppKind`]) are defined here, not
//!   registered from outside. Adding one means adding it to this crate.
//! - **Pure simulation**: no placement, rendering or engine types. Where a built node lives in a
//!   game world is the host's business.
//! - **All or nothing**: every step either succeeds or leaves the world as it was. `build_*`
//!   creates a detached subtree; connecting it is a separate step, so if that fails the node
//!   stays built but unattached. `build_*_at` does both as one step, so if the connection fails
//!   the node is removed again.
//!
//! See `docs/node-templates` in the Smithereen Cold Boot Attack project for the design.

#![forbid(unsafe_code)]

mod computer;
mod npc;

use std::fmt;

use emergence_engine::{ControllerLogic, LinkId, LogicError, NetworkError, NodeId, World};

pub use computer::{
    AppKind, BASE_SERVICES, ComputerSpec, HardwareTag, Kernel, build_computer, build_computer_at,
};
pub use npc::{Npc, NpcRole, NpcSpec, build_npc, build_npc_at, events as npc_events};

/// Where a one-step build (`build_*_at`) attaches the new node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The node to nest it in.
    pub parent: NodeId,
    /// A link owned by (or about to be owned by) `parent` to attach it to, if any.
    pub link: Option<LinkId>,
}

/// Runs `fill` on a freshly created `root`; if it fails, removes `root` and everything created
/// under it, so a failed build leaves no trace.
fn or_remove(
    world: &mut World,
    root: NodeId,
    fill: impl FnOnce(&mut World) -> Result<(), TemplateError>,
) -> Result<NodeId, TemplateError> {
    match fill(world) {
        Ok(()) => Ok(root),
        Err(e) => {
            let _ = world.remove_node(root);
            Err(e)
        }
    }
}

/// Connects a just-built `root` at `at`; if that fails, removes it again.
fn attach_or_remove(
    world: &mut World,
    root: NodeId,
    at: Placement,
) -> Result<NodeId, TemplateError> {
    or_remove(world, root, |world| {
        world
            .network_mut()
            .connect(at.parent, root, at.link)
            .map_err(TemplateError::from)
    })
}

/// Names of the templates, for hosts that pick one at runtime.
pub const TEMPLATES: &[(&str, &str)] = &[
    (
        "computer",
        "A computer: kernel, system services and installed apps on its own bus.",
    ),
    (
        "npc",
        "A non-player character with a schedule, dialogue and a reaction to the player.",
    ),
];

/// Names of the logic kinds this crate provides, accepted by [`create_logic`].
pub const LOGIC_KINDS: &[&str] = &[Kernel::KIND, Npc::KIND];

/// Creates one of this crate's logics with default settings, or `None` if the name is unknown.
#[must_use]
pub fn create_logic(kind: &str) -> Option<Box<dyn ControllerLogic>> {
    match kind {
        Kernel::KIND => Some(Box::new(Kernel::default())),
        Npc::KIND => Some(Box::new(Npc::new(NpcSpec::default()))),
        _ => None,
    }
}

/// Why a template could not be built. Exhaustive: a `match` must handle every case, and a new
/// case is a compile error for callers until they handle it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// An app appears more than once in [`ComputerSpec::apps`].
    AppInstalledTwice(AppKind),
    /// The computer's name is also the name of something inside it (a service or an app).
    NameUsedInside(String),
    /// The [`NpcSpec`] does not make sense.
    Npc(NpcSpecProblem),
    /// The network refused a node, link or connection: an invalid name, a name clash, a
    /// parent or link that does not exist, and so on.
    Network(NetworkError),
    /// Logic could not be attached.
    Logic(LogicError),
}

/// What is wrong with an [`NpcSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpcSpecProblem {
    /// `day_length` is 0.
    ZeroDayLength,
    /// The schedule has no entries.
    EmptySchedule,
    /// The first schedule entry is not at tick 0.
    ScheduleNotFromZero,
    /// Schedule times are not strictly increasing.
    ScheduleOutOfOrder,
    /// A schedule time is not within the day.
    ScheduleOutsideDay,
    /// There are no lines of dialogue.
    NoLines,
}

impl fmt::Display for NpcSpecProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ZeroDayLength => "day_length must be at least 1",
            Self::EmptySchedule => "the schedule needs at least one entry",
            Self::ScheduleNotFromZero => "the schedule must start at tick 0",
            Self::ScheduleOutOfOrder => "schedule times must be in increasing order",
            Self::ScheduleOutsideDay => "schedule times must be within the day",
            Self::NoLines => "an NPC needs at least one line of dialogue",
        })
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppInstalledTwice(app) => write!(f, "`{app}` is installed twice"),
            Self::NameUsedInside(name) => write!(
                f,
                "a computer cannot be called `{name}`: that is the name of something inside it"
            ),
            Self::Npc(problem) => write!(f, "invalid NPC: {problem}"),
            Self::Network(e) => e.fmt(f),
            Self::Logic(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<NetworkError> for TemplateError {
    fn from(e: NetworkError) -> Self {
        Self::Network(e)
    }
}

impl From<LogicError> for TemplateError {
    fn from(e: LogicError) -> Self {
        Self::Logic(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emergence_engine::{Event, TickOutcome, TraceEvent, World};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn names_inside(world: &World, node: emergence_engine::NodeId) -> Vec<String> {
        world
            .network()
            .node(node)
            .map(|n| {
                n.children()
                    .iter()
                    .filter_map(|&c| world.network().node(c).map(|c| c.name().to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_computer_has_its_services_and_apps_and_is_detached() -> TestResult {
        let mut world = World::new();
        let spec = ComputerSpec {
            apps: vec![AppKind::Mail, AppKind::NetScan],
            ..ComputerSpec::default()
        };
        let pc = build_computer(&mut world, "pc", &spec)?;
        let inside = names_inside(&world, pc);
        for &(service, _) in BASE_SERVICES {
            assert!(inside.contains(&service.to_owned()), "missing {service}");
        }
        assert!(inside.contains(&"mail".to_owned()));
        assert!(inside.contains(&"net-scan".to_owned()));
        assert_eq!(
            world
                .network()
                .node(pc)
                .and_then(emergence_engine::ControlNode::parent),
            None
        );
        assert_eq!(world.logic_kind(pc), Some("kernel"));
        Ok(())
    }

    #[test]
    fn an_app_cannot_be_installed_twice() {
        let mut world = World::new();
        let spec = ComputerSpec {
            apps: vec![AppKind::Mail, AppKind::Mail],
            ..ComputerSpec::default()
        };
        assert_eq!(
            build_computer(&mut world, "pc", &spec),
            Err(TemplateError::AppInstalledTwice(AppKind::Mail))
        );
        assert_eq!(world.network().nodes().len(), 1, "nothing was built");
    }

    #[test]
    fn a_built_computer_routes_like_any_other() -> TestResult {
        let mut world = World::new();
        let spec = ComputerSpec {
            apps: vec![AppKind::FileManager],
            ..ComputerSpec::default()
        };
        let pc = build_computer(&mut world, "pc", &spec)?;
        let net = world.network_mut();
        let (root, wifi) = (net.root(), net.create_link("wifi")?);
        let laptop = net.create_node("laptop", "computer")?;
        net.connect(root, pc, Some(wifi))?;
        net.connect(root, laptop, Some(wifi))?;
        world.send(
            laptop,
            wifi,
            "pc@wifi/fileman@ipc".parse()?,
            Event::new("ping"),
        )?;
        for _ in 0..4 {
            assert_eq!(world.tick(), TickOutcome::Completed);
        }
        assert_eq!(world.stats(laptop).received, 1, "the pong came back out");
        Ok(())
    }

    #[test]
    fn a_promiscuous_nic_overhears_traffic() -> TestResult {
        let mut world = World::new();
        let spec = ComputerSpec {
            hardware: [HardwareTag::PromiscuousNic].into(),
            ..ComputerSpec::default()
        };
        let sniffer = build_computer(&mut world, "sniffer", &spec)?;
        let net = world.network_mut();
        let (root, wifi) = (net.root(), net.create_link("wifi")?);
        let (a, b) = (net.create_node("a", "x")?, net.create_node("b", "x")?);
        for n in [sniffer, a, b] {
            net.connect(root, n, Some(wifi))?;
        }
        world.send(a, wifi, "b@wifi".parse()?, Event::new("secret"))?;
        let _ = world.tick();
        assert_eq!(world.stats(sniffer).received, 1);
        Ok(())
    }

    fn notes(world: &mut World) -> Vec<String> {
        world
            .drain_trace()
            .into_iter()
            .filter_map(|e| match e {
                TraceEvent::Note { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_npc_follows_its_schedule_talks_and_reacts() -> TestResult {
        let mut world = World::new();
        let spec = NpcSpec {
            role: NpcRole::Guard,
            day_length: 10,
            schedule: vec![(0, "patrol".into()), (5, "rest".into())],
            lines: vec!["Halt!".into()],
        };
        let guard = build_npc(&mut world, "guard", &spec)?;
        let net = world.network_mut();
        let (root, hall) = (net.root(), net.create_link("hall")?);
        let player = net.create_node("player", "player")?;
        net.connect(root, guard, Some(hall))?;
        net.connect(root, player, Some(hall))?;

        for _ in 0..6 {
            let _ = world.tick();
        }
        let seen = notes(&mut world);
        assert!(
            seen.contains(&"goal: patrol".to_owned()) && seen.contains(&"goal: rest".to_owned())
        );

        world.send(
            player,
            hall,
            "guard@hall".parse()?,
            Event::new(npc_events::TALK),
        )?;
        world.send(
            player,
            hall,
            "guard@hall".parse()?,
            Event::new(npc_events::PLAYER_SEEN),
        )?;
        let _ = world.tick();
        let _ = world.tick();
        assert!(notes(&mut world).iter().any(|n| n.contains("alarm")));
        assert!(
            world.stats(player).received >= 2,
            "got the line and the alarm"
        );
        Ok(())
    }

    #[test]
    fn a_computer_cannot_share_a_name_with_its_insides() {
        let mut world = World::new();
        let spec = ComputerSpec {
            apps: vec![AppKind::Mail],
            ..ComputerSpec::default()
        };
        for name in ["registry", "mail"] {
            assert_eq!(
                build_computer(&mut world, name, &spec),
                Err(TemplateError::NameUsedInside(name.into()))
            );
        }
        assert_eq!(world.network().nodes().len(), 1, "nothing was built");
    }

    #[test]
    fn two_steps_a_refused_connect_leaves_the_node_built_but_detached() -> TestResult {
        let mut world = World::new();
        let net = world.network_mut();
        let (root, wifi) = (net.root(), net.create_link("wifi")?);
        let first = net.create_node("pc", "computer")?;
        net.connect(root, first, Some(wifi))?;

        let pc = build_computer(&mut world, "pc", &ComputerSpec::default())?;
        let refused = world.network_mut().connect(root, pc, Some(wifi));
        assert!(matches!(refused, Err(NetworkError::NameConflict { .. })));
        assert!(world.network().node(pc).is_some(), "still built");
        assert_eq!(
            world
                .network()
                .node(pc)
                .and_then(emergence_engine::ControlNode::parent),
            None
        );
        Ok(())
    }

    #[test]
    fn one_step_a_refused_connect_leaves_nothing() -> TestResult {
        let mut world = World::new();
        let net = world.network_mut();
        let (root, wifi) = (net.root(), net.create_link("wifi")?);
        let first = net.create_node("pc", "computer")?;
        net.connect(root, first, Some(wifi))?;
        let (nodes, links) = (world.network().nodes().len(), world.network().links().len());

        let at = Placement {
            parent: root,
            link: Some(wifi),
        };
        let result = build_computer_at(&mut world, "pc", &ComputerSpec::default(), at);
        assert!(matches!(
            result,
            Err(TemplateError::Network(NetworkError::NameConflict { .. }))
        ));
        assert_eq!(world.network().nodes().len(), nodes, "no nodes left behind");
        assert_eq!(world.network().links().len(), links, "no links left behind");

        let npc = build_npc_at(&mut world, "pc", &NpcSpec::default(), at);
        assert!(npc.is_err());
        assert_eq!(world.network().nodes().len(), nodes);

        let ok = build_computer_at(&mut world, "pc-2", &ComputerSpec::default(), at)?;
        assert_eq!(
            world
                .network()
                .node(ok)
                .and_then(emergence_engine::ControlNode::parent),
            Some(root)
        );
        Ok(())
    }

    #[test]
    fn a_build_that_fails_part_way_removes_what_it_made() -> TestResult {
        let mut world = World::new();
        let before = (world.network().nodes().len(), world.network().links().len());
        let root = world.network_mut().create_node("half-built", "computer")?;
        let result = or_remove(&mut world, root, |world| {
            let net = world.network_mut();
            let bus = net.create_link("bus")?;
            let part = net.create_node("part", "x")?;
            net.connect(root, part, Some(bus))?;
            Err(TemplateError::NameUsedInside("late failure".into()))
        });
        assert!(result.is_err());
        assert_eq!(
            (world.network().nodes().len(), world.network().links().len()),
            before
        );
        Ok(())
    }

    #[test]
    fn bad_npc_specs_are_refused() {
        let mut world = World::new();
        let spec = NpcSpec {
            schedule: vec![(5, "late start".into())],
            ..NpcSpec::default()
        };
        assert_eq!(
            build_npc(&mut world, "npc", &spec),
            Err(TemplateError::Npc(NpcSpecProblem::ScheduleNotFromZero))
        );
    }
}
