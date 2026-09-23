//! Built-in [`ControllerLogic`] implementations, selectable by name.

use crate::route::names;
use crate::{
    AcceptRule, Context, ControllerLogic, Event, Packet, PacketRoute, Relaying, SendError,
};

/// Names of the well-behaved built-in logics, accepted by [`create_logic`].
pub const LOGIC_KINDS: &[&str] = &[
    Responder::KIND,
    Gateway::KIND,
    Bridge::KIND,
    Beacon::KIND,
    Scanner::KIND,
];

/// Names of deliberately broken logics (see [`faulty`]), also accepted by [`create_logic`].
/// They reproduce the bugs that have crashed hosts, for stress testing the engine's defences.
pub const FAULTY_LOGIC_KINDS: &[&str] = &[
    faulty::Echo::KIND,
    faulty::Replayer::KIND,
    faulty::Amplifier::KIND,
    faulty::Flooder::KIND,
    faulty::Crasher::KIND,
];

/// Creates a built-in logic by name, or `None` if the name is unknown.
#[must_use]
pub fn create_logic(kind: &str) -> Option<Box<dyn ControllerLogic>> {
    match kind {
        Responder::KIND => Some(Box::new(Responder)),
        Gateway::KIND => Some(Box::new(Gateway)),
        Bridge::KIND => Some(Box::new(Bridge)),
        Beacon::KIND => Some(Box::new(Beacon::default())),
        Scanner::KIND => Some(Box::new(Scanner::default())),
        faulty::Echo::KIND => Some(Box::new(faulty::Echo)),
        faulty::Replayer::KIND => Some(Box::new(faulty::Replayer::default())),
        faulty::Amplifier::KIND => Some(Box::new(faulty::Amplifier { fan_out: 50 })),
        faulty::Flooder::KIND => Some(Box::new(faulty::Flooder { per_tick: 1_000 })),
        faulty::Crasher::KIND => Some(Box::new(faulty::Crasher)),
        _ => None,
    }
}

/// Event kinds the built-in logics use.
pub mod events {
    /// "Is anyone there?" Any responder answers with [`PONG`].
    pub const PING: &str = "ping";
    /// The answer to [`PING`], carrying the ping's data back.
    pub const PONG: &str = "pong";
}

/// Notes a failed send instead of losing it silently.
fn note_err(ctx: &mut Context<'_>, what: &str, result: Result<(), SendError>) {
    if let Err(e) = result {
        ctx.note(format!("{what} failed: {e}"));
    }
}

/// Answers a ping with a pong. Shared by every built-in logic.
fn answer_ping(packet: &Packet, ctx: &mut Context<'_>) {
    if packet.event().kind == events::PING {
        let pong = Event::with_data(events::PONG, packet.event().data.clone());
        let result = ctx.reply(packet, pong);
        note_err(ctx, "pong", result);
    }
}

/// True on this node's turn in a repeating schedule. Nodes are spread across the interval by
/// name, so they do not all fire on the same tick.
fn on_schedule(ctx: &Context<'_>, interval: u64) -> bool {
    let phase = ctx
        .name()
        .bytes()
        .fold(0_u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    (ctx.tick() + phase).is_multiple_of(interval)
}

/// Answers pings. The simplest useful logic: proves a node is reachable.
#[derive(Debug, Default, Clone, Copy)]
pub struct Responder;

impl Responder {
    /// Name of this logic.
    pub const KIND: &'static str = "responder";
}

impl ControllerLogic for Responder {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        answer_ping(packet, ctx);
    }
}

/// A composite node (a computer, a rack) that connects its internal bus to the outside world.
///
/// - Packets from its children headed outside (accepted by the gateway rule) are forwarded out
///   on the matching external link, with this node added to the return address so replies
///   come back through it.
/// - Multi-hop packets addressed to it have its hop popped and are forwarded inside.
/// - Pings addressed to it are answered.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gateway;

impl Gateway {
    /// Name of this logic.
    pub const KIND: &'static str = "gateway";

    fn forward_out(packet: &Packet, ctx: &mut Context<'_>) {
        let to = packet.to();
        let Some(via) = ctx.external_link_named(to.link()) else {
            ctx.note(format!("no way out to {to}"));
            return;
        };
        let result = ctx
            .via_me(via, packet.from())
            .and_then(|from| ctx.forward(packet, via, from, to.clone()));
        note_err(ctx, "forward out", result);
    }

    fn forward_in(packet: &Packet, next: PacketRoute, ctx: &mut Context<'_>) {
        let Some(via) = ctx.internal_link_named(next.link()) else {
            ctx.note(format!("no internal link for {next}"));
            return;
        };
        let result = ctx.forward(packet, via, packet.from().clone(), next);
        note_err(ctx, "forward in", result);
    }
}

impl ControllerLogic for Gateway {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn relaying(&self) -> Relaying {
        Relaying::Routed
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        match (ctx.accepted_by(), packet.to().popped()) {
            (Some(AcceptRule::Gateway), _) => Self::forward_out(packet, ctx),
            (Some(AcceptRule::Addressed), Some(next)) => Self::forward_in(packet, next, ctx),
            _ => answer_ping(packet, ctx),
        }
    }
}

/// Periodically pings everyone on every link it is attached to, and answers pings itself.
#[derive(Debug, Clone, Copy)]
pub struct Beacon {
    /// Ticks between pings.
    pub interval: u64,
}

impl Beacon {
    /// Name of this logic.
    pub const KIND: &'static str = "beacon";
}

impl Default for Beacon {
    fn default() -> Self {
        Self { interval: 24 }
    }
}

impl ControllerLogic for Beacon {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn on_tick(&mut self, ctx: &mut Context<'_>) {
        if !on_schedule(ctx, self.interval) {
            return;
        }
        let links = ctx
            .network()
            .node(ctx.node())
            .map(|n| n.subscriptions().to_vec())
            .unwrap_or_default();
        for link in links {
            let name = ctx.network().link(link).map_or("", |l| l.name()).to_owned();
            let ping = Event::with_data(events::PING, format!("t{}", ctx.tick()));
            let result = ctx.send(link, PacketRoute::new(names::BROADCAST, name), ping);
            note_err(ctx, "ping", result);
        }
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        answer_ping(packet, ctx);
    }
}

/// An app that scans the networks its computer is on: it periodically broadcasts a ping out
/// through its parent gateway onto each of the parent's external links. The replies travel
/// back through the gateway to the scanner.
#[derive(Debug, Clone, Copy)]
pub struct Scanner {
    /// Ticks between scans.
    pub interval: u64,
}

impl Scanner {
    /// Name of this logic.
    pub const KIND: &'static str = "scanner";
}

impl Default for Scanner {
    fn default() -> Self {
        Self { interval: 40 }
    }
}

impl ControllerLogic for Scanner {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn on_tick(&mut self, ctx: &mut Context<'_>) {
        if !on_schedule(ctx, self.interval) {
            return;
        }
        let net = ctx.network();
        let Some(parent) = net.node(ctx.node()).and_then(crate::ControlNode::parent) else {
            return;
        };
        // The bus shared with the parent: a subscription the parent owns.
        let bus = net.node(ctx.node()).and_then(|n| {
            n.subscriptions()
                .iter()
                .copied()
                .find(|&l| net.link(l).and_then(crate::Link::owner) == Some(parent))
        });
        let Some(bus) = bus else {
            ctx.note("no bus to the parent");
            return;
        };
        let targets: Vec<String> = net
            .node(parent)
            .map(|p| {
                p.subscriptions()
                    .iter()
                    .filter_map(|&l| net.link(l).map(|l| l.name().to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        for target in targets {
            let ping = Event::with_data(events::PING, format!("scan t{}", ctx.tick()));
            let result = ctx.send(bus, PacketRoute::new(names::BROADCAST, target), ping);
            note_err(ctx, "scan", result);
        }
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        answer_ping(packet, ctx);
    }
}

/// Every link this node can transmit on: its subscriptions, then the links it owns.
fn usable_links(ctx: &Context<'_>) -> Vec<crate::LinkId> {
    ctx.network()
        .node(ctx.node())
        .map(|n| {
            n.subscriptions()
                .iter()
                .chain(n.internal_links())
                .copied()
                .collect()
        })
        .unwrap_or_default()
}

/// A hub or bridge: hears everything on every link it is on and passes it, unchanged, to all of
/// its other links. Useful and realistic, but two bridges joining the same links make a loop,
/// and three make a broadcast storm.
#[derive(Debug, Default, Clone, Copy)]
pub struct Bridge;

impl Bridge {
    /// Name of this logic.
    pub const KIND: &'static str = "bridge";
}

impl ControllerLogic for Bridge {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn wants(&self, _packet: &Packet) -> bool {
        true
    }

    fn relaying(&self) -> Relaying {
        Relaying::Flooding
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        if ctx.accepted_by() == Some(AcceptRule::Addressed) {
            answer_ping(packet, ctx);
            return;
        }
        let arrived = ctx.arrived_on();
        for link in usable_links(ctx) {
            if Some(link) != arrived {
                let result = ctx.relay(packet, link);
                note_err(ctx, "relay", result);
            }
        }
    }
}

/// Deliberately broken logics that reproduce real failure shapes, for stress tests.
pub mod faulty {
    use super::{Context, ControllerLogic, Event, Packet, PacketRoute, names, usable_links};
    use crate::LinkId;

    /// Answers every packet with a copy of it. Two echoes ping-pong forever, and because every
    /// reply is a new packet, the TTL never runs out: only the replay check notices.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct Echo;

    impl Echo {
        /// Name of this logic.
        pub const KIND: &'static str = "echo";
    }

    impl ControllerLogic for Echo {
        fn kind(&self) -> &'static str {
            Self::KIND
        }

        fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
            let _ = ctx.reply(packet, packet.event().clone());
        }
    }

    /// A stuck retry: re-sends the first packet it received to its sender, four times every
    /// tick, forever.
    #[derive(Debug, Default, Clone)]
    pub struct Replayer {
        stuck: Option<(LinkId, PacketRoute, Event)>,
    }

    impl Replayer {
        /// Name of this logic.
        pub const KIND: &'static str = "replayer";
    }

    impl ControllerLogic for Replayer {
        fn kind(&self) -> &'static str {
            Self::KIND
        }

        fn on_tick(&mut self, ctx: &mut Context<'_>) {
            if let Some((via, to, event)) = &self.stuck {
                for _ in 0..4 {
                    let _ = ctx.send(*via, to.clone(), event.clone());
                }
            }
        }

        fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
            if self.stuck.is_none()
                && let Some(via) = ctx.arrived_on()
            {
                self.stuck = Some((via, packet.from().clone(), packet.event().clone()));
            }
        }
    }

    /// Answers every packet by broadcasting `fan_out` copies of it on the link it came from.
    #[derive(Debug, Clone, Copy)]
    pub struct Amplifier {
        /// Copies sent per packet received.
        pub fan_out: u32,
    }

    impl Amplifier {
        /// Name of this logic.
        pub const KIND: &'static str = "amplifier";
    }

    impl ControllerLogic for Amplifier {
        fn kind(&self) -> &'static str {
            Self::KIND
        }

        fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
            let Some(via) = ctx.arrived_on() else { return };
            let link = ctx.network().link(via).map_or("", |l| l.name()).to_owned();
            for _ in 0..self.fan_out {
                let to = PacketRoute::new(names::BROADCAST, link.clone());
                if ctx.send(via, to, packet.event().clone()).is_err() {
                    break;
                }
            }
        }
    }

    /// Sends `per_tick` broadcasts on each of its links every tick.
    #[derive(Debug, Clone, Copy)]
    pub struct Flooder {
        /// Packets per link per tick.
        pub per_tick: u32,
    }

    impl Flooder {
        /// Name of this logic.
        pub const KIND: &'static str = "flooder";
    }

    impl ControllerLogic for Flooder {
        fn kind(&self) -> &'static str {
            Self::KIND
        }

        fn on_tick(&mut self, ctx: &mut Context<'_>) {
            for link in usable_links(ctx) {
                let name = ctx.network().link(link).map_or("", |l| l.name()).to_owned();
                for i in 0..self.per_tick {
                    let event = Event::with_data("flood", format!("{}-{i}", ctx.tick()));
                    let to = PacketRoute::new(names::BROADCAST, name.clone());
                    if ctx.send(link, to, event).is_err() {
                        return;
                    }
                }
            }
        }

        fn on_received(&mut self, _packet: &Packet, _ctx: &mut Context<'_>) {}
    }

    /// Panics whenever it receives a packet, like a bug in player-made logic.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct Crasher;

    impl Crasher {
        /// Name of this logic.
        pub const KIND: &'static str = "crasher";
    }

    impl ControllerLogic for Crasher {
        fn kind(&self) -> &'static str {
            Self::KIND
        }

        fn on_received(&mut self, _packet: &Packet, _ctx: &mut Context<'_>) {
            panic!("crasher logic received a packet");
        }
    }
}
