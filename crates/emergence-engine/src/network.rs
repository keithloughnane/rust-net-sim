use std::fmt;

use slotmap::SlotMap;

use crate::{LinkId, NodeId};

/// A node in the network: a device, a service, an app, or a container of other nodes.
///
/// Nodes form a tree. Each node has at most one parent, may own *internal links* for its children
/// to talk over, and may subscribe to any number of links (its own children's links are the
/// usual case, but a node can also join links owned elsewhere, like a laptop joining a Wi-Fi
/// zone).
///
/// Behaviour ("node logic") is not modelled yet; `kind` is a free-form label for now.
#[derive(Debug, Clone)]
pub struct ControlNode {
    name: String,
    kind: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    internal_links: Vec<LinkId>,
    subscriptions: Vec<LinkId>,
}

impl ControlNode {
    /// The node's name, used for routing. Unique among siblings by convention, not enforced.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Free-form label describing what the node is, such as `"computer"` or `"app"`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The node this one is nested inside, if any.
    #[must_use]
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// Nodes nested directly inside this one, in the order they were connected.
    #[must_use]
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    /// Links owned by this node, used by its children to talk to each other.
    #[must_use]
    pub fn internal_links(&self) -> &[LinkId] {
        &self.internal_links
    }

    /// Links this node is attached to, in the order it subscribed.
    #[must_use]
    pub fn subscriptions(&self) -> &[LinkId] {
        &self.subscriptions
    }
}

/// A shared broadcast medium, such as a Wi-Fi zone, a cable, or a computer's internal bus.
///
/// A link is not a point-to-point pipe: any number of nodes can subscribe to it.
#[derive(Debug, Clone)]
pub struct Link {
    name: String,
    owner: Option<NodeId>,
    subscribers: Vec<NodeId>,
}

impl Link {
    /// The link's name, used for routing.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The node this link is internal to. `None` until it is attached to one.
    #[must_use]
    pub fn owner(&self) -> Option<NodeId> {
        self.owner
    }

    /// Nodes attached to this link, in the order they subscribed.
    #[must_use]
    pub fn subscribers(&self) -> &[NodeId] {
        &self.subscribers
    }
}

/// Why a [`Network`] operation was rejected. A rejected operation changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkError {
    /// The node handle does not refer to a node in this network.
    UnknownNode(NodeId),
    /// The link handle does not refer to a link in this network.
    UnknownLink(LinkId),
    /// The node already has a different parent. Disconnect it first.
    AlreadyHasParent(NodeId),
    /// The operation would make a node its own ancestor.
    WouldCreateCycle,
    /// The root node cannot be given a parent.
    IsRoot,
    /// The link is already internal to a different node.
    LinkOwnedElsewhere(LinkId),
    /// The node is not a child of the given parent.
    NotAChild(NodeId),
    /// The name is empty, too long, a reserved token, or contains route syntax.
    /// See [`Network::validate_name`].
    InvalidName(String),
    /// Another node on the same link already has this node's name, so addresses on that link
    /// would be ambiguous.
    NameConflict {
        /// The node being attached.
        node: NodeId,
        /// The node already using the name.
        with: NodeId,
    },
    /// The node already owns, or already subscribes to, another link with this name.
    LinkNameConflict {
        /// The link being attached.
        link: LinkId,
        /// The link already using the name.
        with: LinkId,
    },
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "unknown node {id:?}"),
            Self::UnknownLink(id) => write!(f, "unknown link {id:?}"),
            Self::AlreadyHasParent(id) => write!(f, "node {id:?} already has a parent"),
            Self::WouldCreateCycle => f.write_str("a node cannot be nested inside itself"),
            Self::IsRoot => f.write_str("the root node cannot be moved or removed"),
            Self::LinkOwnedElsewhere(id) => {
                write!(f, "link {id:?} is already internal to another node")
            }
            Self::NotAChild(id) => write!(f, "node {id:?} is not a child of that parent"),
            Self::InvalidName(reason) => write!(f, "invalid name: {reason}"),
            Self::NameConflict { node, with } => {
                write!(
                    f,
                    "node {node:?} has the same name as {with:?} on a shared link"
                )
            }
            Self::LinkNameConflict { link, with } => {
                write!(f, "link {link:?} has the same name as {with:?}")
            }
        }
    }
}

impl std::error::Error for NetworkError {}

/// The whole node/link graph: a tree of [`ControlNode`]s plus the [`Link`]s between them.
///
/// Every network has a root node, named [`Network::ROOT_NAME`], that everything else is
/// ultimately connected under. Links owned by the root are "world" links.
///
/// ```
/// use emergence_engine::Network;
///
/// let mut net = Network::new();
/// let wifi = net.create_link("wifi-1")?;
/// let laptop = net.create_node("laptop", "computer")?;
/// net.connect(net.root(), laptop, Some(wifi))?;
///
/// assert_eq!(net.node(laptop).unwrap().parent(), Some(net.root()));
/// assert_eq!(net.link(wifi).unwrap().subscribers(), &[laptop]);
/// # Ok::<(), emergence_engine::NetworkError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Network {
    nodes: SlotMap<NodeId, ControlNode>,
    links: SlotMap<LinkId, Link>,
    root: NodeId,
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

impl Network {
    /// Name of the root node.
    pub const ROOT_NAME: &'static str = ".";

    /// Creates a network containing only the root node.
    #[must_use]
    pub fn new() -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(ControlNode::new(Self::ROOT_NAME, "root"));
        Self {
            nodes,
            links: SlotMap::with_key(),
            root,
        }
    }

    /// The root node.
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Looks up a node.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&ControlNode> {
        self.nodes.get(id)
    }

    /// Looks up a link.
    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<&Link> {
        self.links.get(id)
    }

    /// All nodes, in creation order.
    #[must_use]
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = (NodeId, &ControlNode)> {
        self.nodes.iter()
    }

    /// All links, in creation order.
    #[must_use]
    pub fn links(&self) -> impl ExactSizeIterator<Item = (LinkId, &Link)> {
        self.links.iter()
    }

    /// Longest allowed node or link name, in characters.
    pub const MAX_NAME_LEN: usize = 64;

    /// Checks a node or link name. Names are addresses, so they must be non-empty, at most
    /// [`MAX_NAME_LEN`](Self::MAX_NAME_LEN) characters, not one of the reserved
    /// [`names`](crate::names), free of route syntax (`@`, `/`) and control characters, and
    /// without leading or trailing whitespace.
    ///
    /// # Errors
    ///
    /// Describes the first problem found.
    pub fn validate_name(name: &str) -> Result<(), NetworkError> {
        use crate::names;
        let problem = if name.is_empty() {
            Some("a name cannot be empty".to_owned())
        } else if name.chars().count() > Self::MAX_NAME_LEN {
            Some(format!(
                "`{name}` is longer than {} characters",
                Self::MAX_NAME_LEN
            ))
        } else if [
            names::ROOT,
            names::PARENT,
            names::BROADCAST,
            names::ANY_LINK,
        ]
        .contains(&name)
        {
            Some(format!("`{name}` is reserved"))
        } else if name.contains(['@', '/']) {
            Some(format!(
                "`{name}` contains `@` or `/`, which are route syntax"
            ))
        } else if name.chars().any(char::is_control) {
            Some("a name cannot contain control characters".to_owned())
        } else if name.trim() != name {
            Some(format!("`{name}` has leading or trailing whitespace"))
        } else {
            None
        };
        problem.map_or(Ok(()), |p| Err(NetworkError::InvalidName(p)))
    }

    /// Creates a detached node. Attach it with [`connect`](Self::connect).
    ///
    /// # Errors
    ///
    /// Fails if the name is invalid (see [`validate_name`](Self::validate_name)).
    pub fn create_node(
        &mut self,
        name: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<NodeId, NetworkError> {
        let name = name.into();
        Self::validate_name(&name)?;
        Ok(self.nodes.insert(ControlNode::new(name, kind)))
    }

    /// Creates a link with no owner and no subscribers.
    ///
    /// # Errors
    ///
    /// Fails if the name is invalid (see [`validate_name`](Self::validate_name)).
    pub fn create_link(&mut self, name: impl Into<String>) -> Result<LinkId, NetworkError> {
        let name = name.into();
        Self::validate_name(&name)?;
        Ok(self.links.insert(Link {
            name,
            owner: None,
            subscribers: Vec::new(),
        }))
    }

    /// Everyone who hears `link`: its subscribers and its owner.
    fn audience(&self, link: LinkId) -> impl Iterator<Item = NodeId> + '_ {
        self.links
            .get(link)
            .into_iter()
            .flat_map(|l| l.subscribers.iter().copied().chain(l.owner))
    }

    /// Fails if `node` joining `link`'s audience would give it a name another listener has.
    fn check_node_name_on(&self, node: NodeId, link: LinkId) -> Result<(), NetworkError> {
        let name = &self.nodes[node].name;
        match self
            .audience(link)
            .find(|&n| n != node && self.nodes.get(n).is_some_and(|o| &o.name == name))
        {
            Some(with) => Err(NetworkError::NameConflict { node, with }),
            None => Ok(()),
        }
    }

    /// Fails if `link` has the same name as one of `others`.
    fn check_link_name_among(&self, link: LinkId, others: &[LinkId]) -> Result<(), NetworkError> {
        let name = &self.links[link].name;
        match others
            .iter()
            .copied()
            .find(|&o| o != link && self.links.get(o).is_some_and(|l| &l.name == name))
        {
            Some(with) => Err(NetworkError::LinkNameConflict { link, with }),
            None => Ok(()),
        }
    }

    /// The checks [`add_internal_link`](Self::add_internal_link) makes, without changing
    /// anything. `Ok(false)` means the link is already internal to `owner`.
    fn check_add_internal_link(&self, owner: NodeId, link: LinkId) -> Result<bool, NetworkError> {
        let owner_node = self.check_node(owner)?;
        match self.check_link(link)?.owner {
            Some(current) if current == owner => return Ok(false),
            Some(_) => return Err(NetworkError::LinkOwnedElsewhere(link)),
            None => {}
        }
        self.check_link_name_among(link, &owner_node.internal_links)?;
        if owner != self.root {
            self.check_node_name_on(owner, link)?;
        }
        Ok(true)
    }

    /// The checks [`subscribe`](Self::subscribe) makes, without changing anything. `Ok(false)`
    /// means the node is already subscribed.
    fn check_subscribe(&self, node: NodeId, link: LinkId) -> Result<bool, NetworkError> {
        let n = self.check_node(node)?;
        if self.check_link(link)?.subscribers.contains(&node) {
            return Ok(false);
        }
        self.check_link_name_among(link, &n.subscriptions)?;
        self.check_node_name_on(node, link)?;
        Ok(true)
    }

    /// Makes `link` internal to `owner`. Does nothing if it already is.
    ///
    /// # Errors
    ///
    /// Fails if either handle is unknown, the link is already internal to another node, the
    /// owner already has an internal link with this name, or a subscriber of the link has the
    /// owner's name.
    pub fn add_internal_link(&mut self, owner: NodeId, link: LinkId) -> Result<(), NetworkError> {
        if !self.check_add_internal_link(owner, link)? {
            return Ok(());
        }
        self.links[link].owner = Some(owner);
        self.nodes[owner].internal_links.push(link);
        Ok(())
    }

    /// Nests `node` inside `parent` and, if `link` is given, makes that link internal to
    /// `parent` and subscribes `node` to it.
    ///
    /// Connecting a node to a parent it already has is allowed, which is how a child is attached
    /// to a second internal link.
    ///
    /// # Errors
    ///
    /// Fails without changing anything if a handle is unknown, `node` is the root, `node`
    /// already has a different parent, the connection would create a cycle, or `link` is
    /// internal to a different node.
    pub fn connect(
        &mut self,
        parent: NodeId,
        node: NodeId,
        link: Option<LinkId>,
    ) -> Result<(), NetworkError> {
        self.check_node(parent)?;
        let current_parent = self.check_node(node)?.parent;
        if node == self.root {
            return Err(NetworkError::IsRoot);
        }
        match current_parent {
            Some(p) if p != parent => return Err(NetworkError::AlreadyHasParent(node)),
            _ => {}
        }
        if self.ancestors_and_self(parent).any(|a| a == node) {
            return Err(NetworkError::WouldCreateCycle);
        }
        if let Some(link) = link {
            self.check_add_internal_link(parent, link)?;
            self.check_subscribe(node, link)?;
            // The owner is about to join the audience too.
            let owner_joins = self.links[link].owner.is_none() && parent != self.root;
            if owner_joins && self.nodes[parent].name == self.nodes[node].name {
                return Err(NetworkError::NameConflict { node, with: parent });
            }
        }

        // Everything is validated; nothing below can fail.
        if current_parent.is_none() {
            self.nodes[node].parent = Some(parent);
            self.nodes[parent].children.push(node);
        }
        if let Some(link) = link {
            self.add_internal_link(parent, link)?;
            self.subscribe(node, link)?;
        }
        Ok(())
    }

    /// Attaches `node` to `link` without changing the hierarchy. Does nothing if it already is.
    ///
    /// # Errors
    ///
    /// Fails if either handle is unknown, another node on the link has this node's name, or
    /// the node already subscribes to a different link with this link's name.
    pub fn subscribe(&mut self, node: NodeId, link: LinkId) -> Result<(), NetworkError> {
        if !self.check_subscribe(node, link)? {
            return Ok(());
        }
        self.links[link].subscribers.push(node);
        self.nodes[node].subscriptions.push(link);
        Ok(())
    }

    /// Detaches `node` from `link`. Does nothing if it was not attached.
    ///
    /// # Errors
    ///
    /// Fails if either handle is unknown.
    pub fn unsubscribe(&mut self, node: NodeId, link: LinkId) -> Result<(), NetworkError> {
        self.check_node(node)?;
        self.check_link(link)?;
        self.links[link].subscribers.retain(|&n| n != node);
        self.nodes[node].subscriptions.retain(|&l| l != link);
        Ok(())
    }

    /// Removes `node` from `parent`, the inverse of [`connect`](Self::connect). The node also
    /// leaves all of `parent`'s internal links. Its own children and other subscriptions are
    /// kept, so it can be connected somewhere else as a whole.
    ///
    /// # Errors
    ///
    /// Fails if either handle is unknown or `node` is not a child of `parent`.
    pub fn disconnect(&mut self, parent: NodeId, node: NodeId) -> Result<(), NetworkError> {
        self.check_node(parent)?;
        if self.check_node(node)?.parent != Some(parent) {
            return Err(NetworkError::NotAChild(node));
        }
        for link in self.nodes[parent].internal_links.clone() {
            self.unsubscribe(node, link)?;
        }
        self.nodes[parent].children.retain(|&c| c != node);
        self.nodes[node].parent = None;
        Ok(())
    }

    /// Deletes `node` and everything nested inside it, with the links they own. Other nodes
    /// subscribed to those links lose that subscription; the node leaves its parent and every
    /// link it was on. Returns every node removed, `node` first.
    ///
    /// Handles to removed nodes and links never resolve again.
    ///
    /// # Errors
    ///
    /// Fails without changing anything if the node is unknown or is the root.
    pub fn remove_node(&mut self, node: NodeId) -> Result<Vec<NodeId>, NetworkError> {
        let parent = self.check_node(node)?.parent;
        if node == self.root {
            return Err(NetworkError::IsRoot);
        }
        // The whole subtree, iteratively so any depth is safe.
        let mut doomed = vec![node];
        let mut i = 0;
        while let Some(&n) = doomed.get(i) {
            doomed.extend(self.nodes[n].children.iter().copied());
            i += 1;
        }
        if let Some(p) = parent {
            self.nodes[p].children.retain(|&c| c != node);
        }
        for &n in &doomed {
            for link in self.nodes[n].subscriptions.clone() {
                if let Some(l) = self.links.get_mut(link) {
                    l.subscribers.retain(|&s| s != n);
                }
            }
            for link in self.nodes[n].internal_links.clone() {
                self.drop_link(link);
            }
        }
        for &n in &doomed {
            self.nodes.remove(n);
        }
        Ok(doomed)
    }

    /// Deletes `link`. Its subscribers lose the subscription and its owner loses the internal
    /// link; the nodes themselves stay.
    ///
    /// # Errors
    ///
    /// Fails if the link is unknown.
    pub fn remove_link(&mut self, link: LinkId) -> Result<(), NetworkError> {
        self.check_link(link)?;
        self.drop_link(link);
        Ok(())
    }

    fn drop_link(&mut self, link: LinkId) {
        let Some(l) = self.links.remove(link) else {
            return;
        };
        for s in l.subscribers {
            if let Some(n) = self.nodes.get_mut(s) {
                n.subscriptions.retain(|&x| x != link);
            }
        }
        if let Some(n) = l.owner.and_then(|o| self.nodes.get_mut(o)) {
            n.internal_links.retain(|&x| x != link);
        }
    }

    /// Whether `node` can transmit on `link`: it subscribes to it or owns it.
    #[must_use]
    pub fn can_use_link(&self, node: NodeId, link: LinkId) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|n| n.subscriptions.contains(&link) || n.internal_links.contains(&link))
    }

    /// The first link called `name` that `node` can transmit on: its subscriptions first, then
    /// the links it owns.
    #[must_use]
    pub fn usable_link_named(&self, node: NodeId, name: &str) -> Option<LinkId> {
        let n = self.nodes.get(node)?;
        n.subscriptions
            .iter()
            .chain(&n.internal_links)
            .copied()
            .find(|&l| self.links.get(l).is_some_and(|l| l.name == name))
    }

    /// `node`, then its parent, then its grandparent, up to the top of its tree.
    pub fn ancestors_and_self(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.nodes.contains_key(node).then_some(node), |&n| {
            self.nodes.get(n).and_then(ControlNode::parent)
        })
    }

    fn check_node(&self, id: NodeId) -> Result<&ControlNode, NetworkError> {
        self.nodes.get(id).ok_or(NetworkError::UnknownNode(id))
    }

    fn check_link(&self, id: LinkId) -> Result<&Link, NetworkError> {
        self.links.get(id).ok_or(NetworkError::UnknownLink(id))
    }
}

impl ControlNode {
    fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            parent: None,
            children: Vec::new(),
            internal_links: Vec::new(),
            subscriptions: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), NetworkError>;

    #[test]
    fn new_network_has_only_the_root() {
        let net = Network::new();
        assert_eq!(net.nodes().len(), 1);
        assert_eq!(net.links().len(), 0);
        assert_eq!(net.node(net.root()).map(ControlNode::name), Some("."));
    }

    #[test]
    fn connect_with_link_builds_hierarchy_and_subscription() -> TestResult {
        let mut net = Network::new();
        let pc = net.create_node("pc", "computer")?;
        let app = net.create_node("app", "app")?;
        let ipc = net.create_link("ipc")?;
        net.connect(net.root(), pc, None)?;
        net.connect(pc, app, Some(ipc))?;

        let pc_node = &net.nodes[pc];
        assert_eq!(pc_node.children(), &[app]);
        assert_eq!(pc_node.internal_links(), &[ipc]);
        assert_eq!(net.nodes[app].parent(), Some(pc));
        assert_eq!(net.nodes[app].subscriptions(), &[ipc]);
        assert_eq!(net.links[ipc].owner(), Some(pc));
        assert_eq!(net.links[ipc].subscribers(), &[app]);
        Ok(())
    }

    #[test]
    fn connecting_twice_to_same_parent_adds_second_link_only() -> TestResult {
        let mut net = Network::new();
        let pc = net.create_node("pc", "computer")?;
        let a = net.create_link("a")?;
        let b = net.create_link("b")?;
        net.connect(net.root(), pc, Some(a))?;
        net.connect(net.root(), pc, Some(b))?;
        assert_eq!(net.nodes[net.root()].children(), &[pc]);
        assert_eq!(net.nodes[pc].subscriptions(), &[a, b]);
        Ok(())
    }

    #[test]
    fn a_node_cannot_have_two_parents() -> TestResult {
        let mut net = Network::new();
        let (p1, p2, child) = (
            net.create_node("p1", "x")?,
            net.create_node("p2", "x")?,
            net.create_node("c", "x")?,
        );
        net.connect(p1, child, None)?;
        assert_eq!(
            net.connect(p2, child, None),
            Err(NetworkError::AlreadyHasParent(child))
        );
        Ok(())
    }

    #[test]
    fn cycles_and_root_reparenting_are_rejected() -> TestResult {
        let mut net = Network::new();
        let a = net.create_node("a", "x")?;
        let b = net.create_node("b", "x")?;
        net.connect(a, b, None)?;
        assert_eq!(net.connect(b, a, None), Err(NetworkError::WouldCreateCycle));
        assert_eq!(net.connect(a, a, None), Err(NetworkError::WouldCreateCycle));
        assert_eq!(net.connect(a, net.root(), None), Err(NetworkError::IsRoot));
        Ok(())
    }

    #[test]
    fn rejected_connect_changes_nothing() -> TestResult {
        let mut net = Network::new();
        let (p1, p2, child) = (
            net.create_node("p1", "x")?,
            net.create_node("p2", "x")?,
            net.create_node("c", "x")?,
        );
        let link = net.create_link("l")?;
        net.add_internal_link(p1, link)?;

        assert_eq!(
            net.connect(p2, child, Some(link)),
            Err(NetworkError::LinkOwnedElsewhere(link))
        );
        assert_eq!(net.nodes[child].parent(), None);
        assert!(net.nodes[p2].children().is_empty());
        assert!(net.links[link].subscribers().is_empty());
        Ok(())
    }

    #[test]
    fn subscribe_to_outside_link_leaves_hierarchy_alone() -> TestResult {
        let mut net = Network::new();
        let wifi = net.create_link("wifi")?;
        let house = net.create_node("house", "building")?;
        let laptop = net.create_node("laptop", "computer")?;
        net.add_internal_link(net.root(), wifi)?;
        net.connect(house, laptop, None)?;
        net.subscribe(laptop, wifi)?;
        net.subscribe(laptop, wifi)?; // idempotent
        assert_eq!(net.nodes[laptop].parent(), Some(house));
        assert_eq!(net.links[wifi].subscribers(), &[laptop]);
        Ok(())
    }

    #[test]
    fn disconnect_leaves_parent_links_but_keeps_everything_else() -> TestResult {
        let mut net = Network::new();
        let pc = net.create_node("pc", "computer")?;
        let app = net.create_node("app", "app")?;
        let ipc = net.create_link("ipc")?;
        let wifi = net.create_link("wifi")?;
        net.connect(pc, app, Some(ipc))?;
        net.subscribe(app, wifi)?;

        net.disconnect(pc, app)?;
        assert_eq!(net.nodes[app].parent(), None);
        assert!(net.nodes[pc].children().is_empty());
        assert_eq!(net.nodes[app].subscriptions(), &[wifi]);
        assert!(net.links[ipc].subscribers().is_empty());
        assert_eq!(net.disconnect(pc, app), Err(NetworkError::NotAChild(app)));
        Ok(())
    }

    #[test]
    fn stale_and_foreign_handles_are_rejected() {
        let mut net = Network::new();
        let bogus = NodeId::from_raw(0xDEAD_BEEF_0000_0001);
        assert_eq!(
            net.connect(net.root(), bogus, None),
            Err(NetworkError::UnknownNode(bogus))
        );
    }

    #[test]
    fn raw_handles_round_trip_and_are_never_zero() -> TestResult {
        let mut net = Network::new();
        let node = net.create_node("n", "x")?;
        assert_ne!(node.to_raw(), 0);
        assert_eq!(NodeId::from_raw(node.to_raw()), node);
        assert!(net.node(NodeId::from_raw(0)).is_none());
        Ok(())
    }

    #[test]
    fn removing_a_node_takes_its_insides_and_their_links() -> TestResult {
        let mut net = Network::new();
        let root = net.root();
        let wifi = net.create_link("wifi")?;
        let pc = net.create_node("pc", "computer")?;
        let laptop = net.create_node("laptop", "computer")?;
        net.connect(root, pc, Some(wifi))?;
        net.connect(root, laptop, Some(wifi))?;
        let ipc = net.create_link("ipc")?;
        let app = net.create_node("app", "app")?;
        net.connect(pc, app, Some(ipc))?;
        // A node outside the computer plugged into its bus.
        net.subscribe(laptop, ipc)?;

        let removed = net.remove_node(pc)?;
        assert_eq!(removed, vec![pc, app]);
        assert!(net.node(pc).is_none() && net.node(app).is_none() && net.link(ipc).is_none());
        assert_eq!(
            net.node(root).map(ControlNode::children),
            Some(&[laptop][..])
        );
        assert_eq!(net.link(wifi).map(Link::subscribers), Some(&[laptop][..]));
        assert_eq!(
            net.node(laptop).map(ControlNode::subscriptions),
            Some(&[wifi][..])
        );
        assert_eq!(net.remove_node(root), Err(NetworkError::IsRoot));
        Ok(())
    }

    #[test]
    fn removing_a_link_keeps_its_nodes() -> TestResult {
        let mut net = Network::new();
        let root = net.root();
        let wifi = net.create_link("wifi")?;
        let pc = net.create_node("pc", "computer")?;
        net.connect(root, pc, Some(wifi))?;
        net.remove_link(wifi)?;
        assert!(net.link(wifi).is_none());
        assert_eq!(net.node(pc).map(|n| n.subscriptions().len()), Some(0));
        assert_eq!(net.node(root).map(|n| n.internal_links().len()), Some(0));
        assert_eq!(net.node(pc).and_then(ControlNode::parent), Some(root));
        Ok(())
    }

    #[test]
    fn invalid_names_are_rejected() {
        let mut net = Network::new();
        for bad in [
            "",
            "*",
            "^",
            "?",
            ".",
            "a@b",
            "a/b",
            " pad",
            "tab\t",
            &"x".repeat(65),
        ] {
            assert!(
                matches!(net.create_node(bad, "x"), Err(NetworkError::InvalidName(_))),
                "node name {bad:?} was accepted"
            );
            assert!(
                matches!(net.create_link(bad), Err(NetworkError::InvalidName(_))),
                "link name {bad:?} was accepted"
            );
        }
        assert!(net.create_node("⊙PAN device", "x").is_ok());
    }

    #[test]
    fn same_name_is_fine_in_separate_computers_but_not_on_one_link() -> TestResult {
        let mut net = Network::new();
        let root = net.root();
        let wifi = net.create_link("wifi")?;
        let (pc1, pc2) = (
            net.create_node("pc-1", "computer")?,
            net.create_node("pc-2", "computer")?,
        );
        net.connect(root, pc1, Some(wifi))?;
        net.connect(root, pc2, Some(wifi))?;
        for pc in [pc1, pc2] {
            let ipc = net.create_link("ipc")?;
            let registry = net.create_node("registry", "service")?;
            net.connect(pc, registry, Some(ipc))?;
        }

        let dup = net.create_node("pc-1", "computer")?;
        assert_eq!(
            net.connect(root, dup, Some(wifi)),
            Err(NetworkError::NameConflict {
                node: dup,
                with: pc1
            })
        );
        assert_eq!(
            net.node(dup).and_then(ControlNode::parent),
            None,
            "rejected connect changed nothing"
        );
        Ok(())
    }

    #[test]
    fn an_owner_cannot_share_a_name_with_its_link_subscribers() -> TestResult {
        let mut net = Network::new();
        let pc = net.create_node("pc", "computer")?;
        let app = net.create_node("pc", "app")?;
        let ipc = net.create_link("ipc")?;
        assert_eq!(
            net.connect(pc, app, Some(ipc)),
            Err(NetworkError::NameConflict {
                node: app,
                with: pc
            })
        );
        Ok(())
    }

    #[test]
    fn link_names_are_unique_per_owner_and_per_subscriber() -> TestResult {
        let mut net = Network::new();
        let pc = net.create_node("pc", "computer")?;
        let (a, b) = (net.create_link("ipc")?, net.create_link("ipc")?);
        net.add_internal_link(pc, a)?;
        assert_eq!(
            net.add_internal_link(pc, b),
            Err(NetworkError::LinkNameConflict { link: b, with: a })
        );
        let dev = net.create_node("dev", "x")?;
        let (w1, w2) = (net.create_link("wifi")?, net.create_link("wifi")?);
        net.subscribe(dev, w1)?;
        assert_eq!(
            net.subscribe(dev, w2),
            Err(NetworkError::LinkNameConflict { link: w2, with: w1 })
        );
        Ok(())
    }
}
