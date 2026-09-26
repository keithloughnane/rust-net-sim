//! The computer template: a kernel root, a fixed set of system services and the installed apps,
//! all on the computer's own `ipc` bus.

use std::collections::BTreeSet;
use std::fmt;

use emergence_engine::{
    AcceptRule, Context, ControllerLogic, Gateway, NodeId, Packet, Relaying, Responder, Scanner,
    World, names,
};

use crate::TemplateError;

/// The services every computer has. They are what "being a computer" means, not options.
pub const BASE_SERVICES: &[(&str, &str)] = &[
    ("login-manager", "session gate: is anyone logged in"),
    ("registry", "the machine's settings"),
    ("desktop", "the desktop session and taskbar"),
    ("hid-serv", "input routing and window focus"),
    ("drive-bay", "storage: the boot disk and files"),
];

/// The apps a computer can have installed. A fixed catalogue: adding an app means adding it
/// here, not registering it from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum AppKind {
    /// Browse and move files.
    #[cfg_attr(feature = "serde", serde(rename = "fileman"))]
    FileManager,
    /// Scan the networks the computer is on.
    NetScan,
    /// Email.
    Mail,
    /// Calendar.
    Calendar,
    /// Web browser.
    Browser,
    /// Command line.
    Terminal,
    /// Bulletin-board client.
    BbsClient,
    /// Modem dialler.
    Dialer,
    /// Password cracker.
    CryptCracker,
    /// Shares files with other computers.
    FileShare,
    /// A user directory service.
    Directory,
    /// Runs jobs on a schedule.
    Scheduler,
    /// A database server.
    Database,
}

impl AppKind {
    /// Every app in the catalogue.
    pub const ALL: &[Self] = &[
        Self::FileManager,
        Self::NetScan,
        Self::Mail,
        Self::Calendar,
        Self::Browser,
        Self::Terminal,
        Self::BbsClient,
        Self::Dialer,
        Self::CryptCracker,
        Self::FileShare,
        Self::Directory,
        Self::Scheduler,
        Self::Database,
    ];

    /// The app's node name inside a computer, such as `"net-scan"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::FileManager => "fileman",
            Self::NetScan => "net-scan",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Browser => "browser",
            Self::Terminal => "terminal",
            Self::BbsClient => "bbs-client",
            Self::Dialer => "dialer",
            Self::CryptCracker => "crypt-cracker",
            Self::FileShare => "file-share",
            Self::Directory => "directory",
            Self::Scheduler => "scheduler",
            Self::Database => "database",
        }
    }

    /// A short description for people.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::FileManager => "File manager",
            Self::NetScan => "Network scanner",
            Self::Mail => "Mail",
            Self::Calendar => "Calendar",
            Self::Browser => "Browser",
            Self::Terminal => "Terminal",
            Self::BbsClient => "BBS client",
            Self::Dialer => "Dialer",
            Self::CryptCracker => "Crypt cracker",
            Self::FileShare => "File share",
            Self::Directory => "Directory service",
            Self::Scheduler => "Scheduler",
            Self::Database => "Database",
        }
    }

    /// Looks an app up by [`name`](Self::name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.name() == name)
    }

    /// The logic this app runs. Apps' own puzzle logic is not modelled yet; they answer pings,
    /// and the network scanner scans.
    fn logic(self) -> Box<dyn ControllerLogic> {
        match self {
            Self::NetScan => Box::new(Scanner::default()),
            Self::FileManager
            | Self::Mail
            | Self::Calendar
            | Self::Browser
            | Self::Terminal
            | Self::BbsClient
            | Self::Dialer
            | Self::CryptCracker
            | Self::FileShare
            | Self::Directory
            | Self::Scheduler
            | Self::Database => Box::new(Responder),
        }
    }
}

impl fmt::Display for AppKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A hardware capability a computer can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum HardwareTag {
    /// A Wi-Fi card.
    Wifi,
    /// A modem.
    Modem,
    /// A network card in promiscuous mode: the kernel hears all traffic on its links, not just
    /// what is addressed to it.
    PromiscuousNic,
}

impl HardwareTag {
    /// Every tag.
    pub const ALL: &[Self] = &[Self::Wifi, Self::Modem, Self::PromiscuousNic];

    /// The tag's name, such as `"promiscuous-nic"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Modem => "modem",
            Self::PromiscuousNic => "promiscuous-nic",
        }
    }
}

/// Parameters for [`build_computer`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(default, deny_unknown_fields))]
pub struct ComputerSpec {
    /// Installed apps, from the fixed catalogue. Each at most once.
    pub apps: Vec<AppKind>,
    /// Hardware capabilities.
    pub hardware: BTreeSet<HardwareTag>,
}

/// Builds a computer and returns its root, detached: connect it wherever it belongs (or use
/// [`build_computer_at`] to build and connect in one step).
///
/// Atomic: if it fails, nothing is left in the world.
///
/// Inside: a `kernel` root that is the computer's gateway, an `ipc` bus, the
/// [`BASE_SERVICES`], and one node per installed app. Callers should treat the insides as
/// private; they may grow.
///
/// ```
/// use emergence_engine::World;
/// use emergence_templates::{AppKind, ComputerSpec, build_computer};
///
/// let mut world = World::new();
/// let spec = ComputerSpec { apps: vec![AppKind::Mail, AppKind::NetScan], ..Default::default() };
/// let pc = build_computer(&mut world, "pc-1", &spec)?;
///
/// let wifi = world.network_mut().create_link("wifi")?;
/// let root = world.network().root();
/// world.network_mut().connect(root, pc, Some(wifi))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Fails, without building anything, if the name is invalid or used by something inside the
/// computer, or an app is listed twice.
pub fn build_computer(
    world: &mut World,
    name: &str,
    spec: &ComputerSpec,
) -> Result<NodeId, TemplateError> {
    emergence_engine::Network::validate_name(name)?;
    let inside = BASE_SERVICES
        .iter()
        .map(|&(service, _)| service)
        .chain(spec.apps.iter().map(|a| a.name()));
    if inside.clone().any(|n| n == name) {
        return Err(TemplateError::NameUsedInside(name.to_owned()));
    }
    let mut seen = BTreeSet::new();
    if let Some(&twice) = spec.apps.iter().find(|&&app| !seen.insert(app)) {
        return Err(TemplateError::AppInstalledTwice(twice));
    }

    let root = world.network_mut().create_node(name, "computer")?;
    crate::or_remove(world, root, |world| fill_computer(world, root, spec))
}

/// Everything inside a computer. On error, the caller removes `root` and all of this with it.
fn fill_computer(
    world: &mut World,
    root: NodeId,
    spec: &ComputerSpec,
) -> Result<(), TemplateError> {
    let net = world.network_mut();
    let ipc = net.create_link(names::IPC)?;
    if let Err(e) = net.add_internal_link(root, ipc) {
        let _ = net.remove_link(ipc);
        return Err(e.into());
    }
    let parts = BASE_SERVICES
        .iter()
        .map(|&(service, _)| {
            (
                service,
                "service",
                Box::new(Responder) as Box<dyn ControllerLogic>,
            )
        })
        .chain(
            spec.apps
                .iter()
                .map(|&app| (app.name(), "app", app.logic())),
        );
    for (name, kind, logic) in parts {
        let node = world.network_mut().create_node(name, kind)?;
        // Once connected, it is inside `root` and goes if `root` goes; until then, clean up here.
        if let Err(e) = world.network_mut().connect(root, node, Some(ipc)) {
            let _ = world.remove_node(node);
            return Err(e.into());
        }
        world.set_logic(node, logic)?;
    }
    world.set_logic(root, Box::new(Kernel::new(&spec.hardware)))?;
    Ok(())
}

/// Builds a computer and connects it in one step: nested in `at.parent`, on `at.link`. If
/// either part fails, nothing is left behind.
///
/// # Errors
///
/// Fails if [`build_computer`] or the connection fails; the world is then unchanged.
pub fn build_computer_at(
    world: &mut World,
    name: &str,
    spec: &ComputerSpec,
    at: crate::Placement,
) -> Result<NodeId, TemplateError> {
    let root = build_computer(world, name, spec)?;
    crate::attach_or_remove(world, root, at)
}

/// A computer's root logic: the gateway between its apps and the networks it is on, plus
/// whatever its hardware adds.
#[derive(Debug, Default, Clone)]
pub struct Kernel {
    promiscuous: bool,
    sniffed: u64,
}

impl Kernel {
    /// Name of this logic.
    pub const KIND: &'static str = "kernel";

    /// A kernel for the given hardware.
    #[must_use]
    pub fn new(hardware: &BTreeSet<HardwareTag>) -> Self {
        Self {
            promiscuous: hardware.contains(&HardwareTag::PromiscuousNic),
            sniffed: 0,
        }
    }

    /// Packets overheard by a promiscuous network card.
    #[must_use]
    pub fn sniffed(&self) -> u64 {
        self.sniffed
    }
}

impl ControllerLogic for Kernel {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn relaying(&self) -> Relaying {
        Relaying::Routed
    }

    fn wants(&self, _packet: &Packet) -> bool {
        self.promiscuous
    }

    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
        if ctx.accepted_by() == Some(AcceptRule::Forced) {
            // Overheard, not addressed to us: a sniffer records it and does nothing else.
            self.sniffed += 1;
            if self.sniffed.is_power_of_two() {
                ctx.note(format!(
                    "sniffed {} packets (latest: {} → {})",
                    self.sniffed,
                    packet.event().kind,
                    packet.to()
                ));
            }
            return;
        }
        Gateway.on_received(packet, ctx);
    }
}
