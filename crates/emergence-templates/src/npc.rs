//! The NPC template. Today one flat node, as in the design docs' MVP: a schedule, dialogue, and
//! a reaction to being seen. Memory and inventory sub-nodes can be added later without changing
//! how callers build or use an NPC.

use emergence_engine::{
    Context, ControllerLogic, Event, LinkId, NodeId, Packet, PacketRoute, World, builtin_events,
    names,
};

use crate::TemplateError;

/// Event kinds NPCs understand and send.
pub mod events {
    /// Sent to an NPC to start or continue a conversation.
    pub const TALK: &str = "talk";
    /// An NPC's line of dialogue, sent back to whoever talked to it.
    pub const SAY: &str = "say";
    /// Sent to an NPC by the host when it sees the player. Detection itself is the host's job
    /// (it is spatial); reacting to it is the NPC's.
    pub const PLAYER_SEEN: &str = "player-seen";
    /// Broadcast by a guard that has seen the player.
    pub const ALARM: &str = "alarm";
    /// Broadcast whenever an NPC's goal changes, so the host can move it.
    pub const GOAL: &str = "goal";
}

/// What kind of NPC this is, which decides how it reacts to seeing the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum NpcRole {
    /// Raises the alarm when it sees the player.
    Guard,
    /// Flees when it sees the player.
    #[default]
    Civilian,
}

impl NpcRole {
    /// Every role.
    pub const ALL: &[Self] = &[Self::Guard, Self::Civilian];

    /// The role's name, such as `"guard"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Guard => "guard",
            Self::Civilian => "civilian",
        }
    }
}

/// Parameters for [`build_npc`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default, deny_unknown_fields))]
pub struct NpcSpec {
    /// How it reacts to seeing the player.
    pub role: NpcRole,
    /// Ticks in one day of its schedule.
    pub day_length: u64,
    /// `(tick of the day, goal)` pairs in order, starting at 0. The goal is a free-form name
    /// the host turns into movement, such as `"patrol"` or `"sleep"`.
    pub schedule: Vec<(u64, String)>,
    /// What it says, in order, when talked to. It cycles.
    pub lines: Vec<String>,
}

impl Default for NpcSpec {
    fn default() -> Self {
        Self {
            role: NpcRole::default(),
            day_length: 240,
            schedule: vec![
                (0, "sleep".into()),
                (60, "work".into()),
                (180, "relax".into()),
            ],
            lines: vec!["Hello.".into(), "Nice weather today.".into()],
        }
    }
}

impl NpcSpec {
    fn validate(&self) -> Result<(), TemplateError> {
        use crate::NpcSpecProblem as P;
        let problem = if self.day_length == 0 {
            Some(P::ZeroDayLength)
        } else if self.schedule.is_empty() {
            Some(P::EmptySchedule)
        } else if self.schedule[0].0 != 0 {
            Some(P::ScheduleNotFromZero)
        } else if self.schedule.windows(2).any(|w| w[0].0 >= w[1].0) {
            Some(P::ScheduleOutOfOrder)
        } else if self.schedule.iter().any(|(t, _)| *t >= self.day_length) {
            Some(P::ScheduleOutsideDay)
        } else if self.lines.is_empty() {
            Some(P::NoLines)
        } else {
            None
        };
        problem.map_or(Ok(()), |p| Err(TemplateError::Npc(p)))
    }
}

/// Builds an NPC and returns its node, detached: connect it to the links it should hear (or use
/// [`build_npc_at`] to build and connect in one step).
///
/// # Errors
///
/// Fails, without building anything, if the name or spec is invalid.
pub fn build_npc(world: &mut World, name: &str, spec: &NpcSpec) -> Result<NodeId, TemplateError> {
    emergence_engine::Network::validate_name(name)?;
    spec.validate()?;
    let node = world.network_mut().create_node(name, "npc")?;
    crate::or_remove(world, node, |world| {
        world.set_logic(node, Box::new(Npc::new(spec.clone())))?;
        Ok(())
    })
}

/// Builds an NPC and connects it in one step. If either part fails, nothing is left behind.
///
/// # Errors
///
/// Fails if [`build_npc`] or the connection fails; the world is then unchanged.
pub fn build_npc_at(
    world: &mut World,
    name: &str,
    spec: &NpcSpec,
    at: crate::Placement,
) -> Result<NodeId, TemplateError> {
    let node = build_npc(world, name, spec)?;
    crate::attach_or_remove(world, node, at)
}

/// An NPC's logic.
#[derive(Debug, Clone)]
pub struct Npc {
    spec: NpcSpec,
    goal: Option<String>,
    next_line: usize,
}

impl Npc {
    /// Name of this logic.
    pub const KIND: &'static str = "npc";

    /// An NPC following `spec`.
    #[must_use]
    pub fn new(spec: NpcSpec) -> Self {
        Self {
            spec,
            goal: None,
            next_line: 0,
        }
    }

    /// Its current goal, once the schedule has started.
    #[must_use]
    pub fn goal(&self) -> Option<&str> {
        self.goal.as_deref()
    }

    fn set_goal(&mut self, goal: &str, ctx: &mut Context<'_>) {
        if self.goal.as_deref() == Some(goal) {
            return;
        }
        self.goal = Some(goal.to_owned());
        ctx.note(format!("goal: {goal}"));
        broadcast(ctx, &Event::with_data(events::GOAL, goal));
    }
}

/// Sends `event` to everyone on every link the node is on.
fn broadcast(ctx: &mut Context<'_>, event: &Event) {
    let links: Vec<LinkId> = ctx
        .network()
        .node(ctx.node())
        .map(|n| n.subscriptions().to_vec())
        .unwrap_or_default();
    for link in links {
        let name = ctx.network().link(link).map_or("", |l| l.name()).to_owned();
        let _ = ctx.send(
            link,
            PacketRoute::new(names::BROADCAST, name),
            event.clone(),
        );
    }
}

impl ControllerLogic for Npc {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn on_tick(&mut self, ctx: &mut Context<'_>) {
        let time = ctx.tick() % self.spec.day_length;
        let goal = self
            .spec
            .schedule
            .iter()
            .rev()
            .find(|(start, _)| *start <= time)
            .map(|(_, goal)| goal.clone());
        // "flee" overrides the schedule until the next scheduled change.
        let fleeing = self.goal.as_deref() == Some("flee");
        let scheduled_change = self.spec.schedule.iter().any(|(start, _)| *start == time);
        if let Some(goal) = goal
            && (!fleeing || scheduled_change)
        {
            self.set_goal(&goal, ctx);
        }
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        match packet.event().kind.as_str() {
            events::TALK => {
                let line = self.spec.lines[self.next_line % self.spec.lines.len()].clone();
                self.next_line += 1;
                let _ = ctx.reply(packet, Event::with_data(events::SAY, line));
            }
            events::PLAYER_SEEN => match self.spec.role {
                NpcRole::Guard => {
                    let who = ctx.name().to_owned();
                    ctx.note("saw the player: raising the alarm");
                    broadcast(ctx, &Event::with_data(events::ALARM, who));
                }
                NpcRole::Civilian => self.set_goal("flee", ctx),
            },
            builtin_events::PING => {
                let pong = Event::with_data(builtin_events::PONG, packet.event().data.clone());
                let _ = ctx.reply(packet, pong);
            }
            _ => {}
        }
    }
}
