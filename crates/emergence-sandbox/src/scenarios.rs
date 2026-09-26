//! Test networks, built entirely through the native library's C ABI.

use crate::native::{LinkId, NativeError, NativeWorld, NodeId};

pub(crate) type BuildResult = Result<(), NativeError>;

/// A named test network.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scenario {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    build: fn(&mut Builder<'_>) -> BuildResult,
}

impl Scenario {
    pub(crate) fn build(&self, world: &mut NativeWorld) -> BuildResult {
        (self.build)(&mut Builder::new(world)?)
    }
}

pub(crate) const ALL: &[Scenario] = &[
    Scenario {
        name: "Office",
        description: "A small office: LAN, Wi-Fi, a server rack, the player's inventory, NPCs.",
        build: office,
    },
    Scenario {
        name: "Single computer",
        description: "One computer with a full set of apps, one of them hosting a VM.",
        build: single_computer,
    },
    Scenario {
        name: "City block",
        description: "Eight buildings of networked computers: a few hundred nodes, deep nesting.",
        build: city_block,
    },
    Scenario {
        name: "Mesh",
        description: "Thirty devices on ten shared links at one level, to stress the layout.",
        build: mesh,
    },
];

// Built-in logic kinds (see `emergence_engine::LOGIC_KINDS`).
const RESPONDER: &str = "responder";
const GATEWAY: &str = "gateway";
const BEACON: &str = "beacon";
const SCANNER: &str = "scanner";

/// Convenience layer over [`NativeWorld`] for building networks.
pub(crate) struct Builder<'w> {
    pub(crate) world: &'w mut NativeWorld,
    pub(crate) root: NodeId,
}

impl<'w> Builder<'w> {
    pub(crate) fn new(world: &'w mut NativeWorld) -> Result<Self, NativeError> {
        let root = world.root()?;
        Ok(Self { world, root })
    }

    /// A link owned by the root: a Wi-Fi zone, a phone line, a street.
    pub(crate) fn world_link(&mut self, name: &str) -> Result<LinkId, NativeError> {
        let link = self.world.create_link(name)?;
        self.world.add_internal_link(self.root, link)?;
        Ok(link)
    }

    /// A link owned by `owner`, for its children.
    pub(crate) fn internal_link(
        &mut self,
        owner: NodeId,
        name: &str,
    ) -> Result<LinkId, NativeError> {
        let link = self.world.create_link(name)?;
        self.world.add_internal_link(owner, link)?;
        Ok(link)
    }

    /// A node nested in `parent` and attached to each of `links` (the first also becomes one of
    /// `parent`'s internal links if it is not already).
    pub(crate) fn node(
        &mut self,
        parent: NodeId,
        name: &str,
        kind: &str,
        links: &[LinkId],
    ) -> Result<NodeId, NativeError> {
        let node = self.world.create_node(name, kind)?;
        self.world.connect(parent, node, links.first().copied())?;
        for &link in links.iter().skip(1) {
            self.world.subscribe(node, link)?;
        }
        Ok(node)
    }

    /// Attaches a built-in logic.
    pub(crate) fn logic(&mut self, node: NodeId, kind: &str) -> BuildResult {
        self.world.set_logic(node, kind)
    }

    /// A node with a logic attached.
    pub(crate) fn device(
        &mut self,
        parent: NodeId,
        name: &str,
        kind: &str,
        links: &[LinkId],
        logic: &str,
    ) -> Result<NodeId, NativeError> {
        let node = self.node(parent, name, kind, links)?;
        self.logic(node, logic)?;
        Ok(node)
    }

    /// A computer from the templates library, nested in `parent` and attached to `links` (the
    /// first also becomes one of `parent`'s internal links). `apps` are catalogue names.
    pub(crate) fn computer(
        &mut self,
        parent: NodeId,
        name: &str,
        links: &[LinkId],
        apps: &[&str],
    ) -> Result<NodeId, NativeError> {
        let spec = serde_json::json!({ "apps": apps }).to_string();
        let pc = self.world.build_template("computer", name, &spec)?;
        self.world.connect(parent, pc, links.first().copied())?;
        for &link in links.iter().skip(1) {
            self.world.subscribe(pc, link)?;
        }
        Ok(pc)
    }

    /// An NPC from the templates library, attached to `links`.
    pub(crate) fn npc(
        &mut self,
        name: &str,
        links: &[LinkId],
        spec: &serde_json::Value,
    ) -> Result<NodeId, NativeError> {
        let npc = self.world.build_template("npc", name, &spec.to_string())?;
        self.world.connect(self.root, npc, links.first().copied())?;
        for &link in links.iter().skip(1) {
            self.world.subscribe(npc, link)?;
        }
        Ok(npc)
    }

    /// A computer's own bus. Scenarios normally treat a template's insides as private; this is
    /// for the few that deliberately plug something extra in.
    pub(crate) fn bus_of(&mut self, computer: NodeId) -> Result<LinkId, NativeError> {
        self.world
            .snapshot()?
            .node(computer)
            .and_then(|n| n.internal_links.first().copied())
            .ok_or_else(|| NativeError::Sandbox("the computer has no bus".into()))
    }
}

fn office(b: &mut Builder<'_>) -> BuildResult {
    let root = b.root;
    let lan = b.world_link("office-lan")?;
    let wifi = b.world_link("office-wifi")?;
    let street = b.world_link("street-wifi")?;
    let phone_line = b.world_link("phone-line")?;
    let isp = b.world_link("isp-uplink")?;
    let npc_range = b.world_link("--")?;
    b.world_link("spare-cable")?; // Owned but unused, to show an empty link.

    b.device(root, "router", "router", &[lan, wifi, isp], BEACON)?;
    b.device(root, "modem", "modem", &[phone_line, isp], RESPONDER)?;
    b.computer(root, "pc-reception", &[lan], &["mail", "calendar"])?;
    b.computer(
        root,
        "pc-manager",
        &[wifi],
        &["mail", "fileman", "net-scan"],
    )?;
    b.computer(root, "laptop", &[wifi, street], &["browser"])?;

    // A rack groups servers behind its own backplane, one level deeper.
    let rack = b.device(root, "server-rack", "rack", &[lan], GATEWAY)?;
    let backplane = b.internal_link(rack, "backplane")?;
    b.computer(rack, "srv-files", &[backplane], &["file-share"])?;
    b.computer(rack, "srv-auth", &[backplane], &["directory"])?;
    b.computer(rack, "srv-backup", &[backplane], &["scheduler"])?;

    b.device(root, "desk-phone", "phone", &[phone_line], BEACON)?;

    // The player carries a personal-area network: an inventory with devices inside it.
    let player = b.device(root, "player", "player", &[street], GATEWAY)?;
    let pan = b.internal_link(player, "⊙PAN")?;
    let inventory = b.device(player, "inventory", "inventory", &[pan], GATEWAY)?;
    let pocket = b.internal_link(inventory, "pocket")?;
    b.device(inventory, "pager", "device", &[pocket], RESPONDER)?;
    b.device(inventory, "rf-scanner", "device", &[pocket], SCANNER)?;
    b.device(player, "phone", "phone", &[pan, street], BEACON)?;

    let guard = serde_json::json!({
        "role": "guard",
        "day_length": 96,
        "schedule": [[0, "patrol"], [48, "check-doors"]],
        "lines": ["Move along.", "Office is closed after six."],
    });
    b.npc("npc-guard", &[npc_range, street], &guard)?;
    b.npc(
        "npc-cleaner",
        &[npc_range],
        &serde_json::json!({ "role": "civilian" }),
    )?;
    Ok(())
}

fn single_computer(b: &mut Builder<'_>) -> BuildResult {
    let wifi = b.world_link("wifi-1")?;
    let pc = b.computer(
        b.root,
        "workstation",
        &[wifi],
        &["fileman", "terminal", "browser", "mail", "net-scan"],
    )?;
    // A VM host plugged into the computer's bus, with a whole computer inside it.
    let ipc = b.bus_of(pc)?;
    let host = b.device(pc, "vm-host", "app", &[ipc], GATEWAY)?;
    let vm_net = b.internal_link(host, "vm-net")?;
    b.computer(host, "vm-guest", &[vm_net], &["database", "net-scan"])?;
    Ok(())
}

fn city_block(b: &mut Builder<'_>) -> BuildResult {
    let root = b.root;
    let street_a = b.world_link("street-wifi-a")?;
    let street_b = b.world_link("street-wifi-b")?;
    let fibre = b.world_link("fibre")?;
    for building in 0..8 {
        let street = if building < 4 { street_a } else { street_b };
        let name = format!("building-{}", building + 1);
        let bldg = b.device(root, &name, "building", &[fibre, street], GATEWAY)?;
        let lan = b.internal_link(bldg, "lan")?;
        for floor in 0..6 {
            b.computer(
                bldg,
                &format!("pc-{}{:02}", building + 1, floor + 1),
                &[lan],
                if floor == 0 {
                    &["mail", "net-scan"]
                } else {
                    &["mail"]
                },
            )?;
        }
    }
    Ok(())
}

fn mesh(b: &mut Builder<'_>) -> BuildResult {
    let links = (0..10)
        .map(|i| b.world_link(&format!("net-{i}")))
        .collect::<Result<Vec<_>, _>>()?;
    // Deterministic pseudo-random subscriptions (a tiny LCG), so the scenario is reproducible.
    let mut seed: u32 = 0x5EED;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (seed >> 16) as usize % n
    };
    for i in 0..30 {
        let count = 1 + next(3);
        let mut mine: Vec<LinkId> = (0..count).map(|_| links[next(links.len())]).collect();
        mine.sort_unstable();
        mine.dedup();
        let logic = if i % 6 == 0 { BEACON } else { RESPONDER };
        b.device(b.root, &format!("device-{i:02}"), "device", &mine, logic)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::NativeLibrary;

    /// Builds every scenario through the real compiled library (`cargo build` first).
    #[test]
    fn every_scenario_builds() -> Result<(), NativeError> {
        let library = NativeLibrary::load(&NativeLibrary::default_path())?;
        for scenario in ALL {
            let mut world = NativeWorld::new(library.clone())?;
            scenario.build(&mut world)?;
            let snap = world.snapshot()?;
            assert!(snap.node_count() > 1, "{} built nothing", scenario.name);
        }
        Ok(())
    }
}
