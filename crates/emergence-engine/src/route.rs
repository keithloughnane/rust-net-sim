use std::fmt;
use std::str::FromStr;

/// Reserved node and link names with special meaning in routes.
pub mod names {
    /// Name of the network's root node.
    pub const ROOT: &str = ".";
    /// As a destination node: "my parent", resolved structurally by whoever owns the link.
    pub const PARENT: &str = "^";
    /// As a destination node or link: everyone.
    pub const BROADCAST: &str = "*";
    /// As a link: "whichever link this arrives on".
    pub const ANY_LINK: &str = "?";
    /// Conventional name of a composite node's own internal bus.
    pub const IPC: &str = "ipc";
}

/// One step of a route: deliver to `node`, arriving over `link`. Both are names, not handles,
/// because addresses travel inside packets and are resolved hop by hop.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hop {
    /// Node name, or one of the reserved [`names`].
    pub node: String,
    /// Link name, or one of the reserved [`names`].
    pub link: String,
}

impl Hop {
    /// Creates a hop.
    pub fn new(node: impl Into<String>, link: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            link: link.into(),
        }
    }
}

/// Why a route could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteError {
    /// A route needs at least one hop.
    Empty,
    /// More than [`PacketRoute::MAX_DEPTH`] hops.
    TooDeep,
    /// A hop in route text had an empty node or link name.
    BadHop(String),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a route needs at least one hop"),
            Self::TooDeep => write!(f, "a route has at most {} hops", PacketRoute::MAX_DEPTH),
            Self::BadHop(hop) => write!(f, "invalid hop `{hop}`: expected `node@link`"),
        }
    }
}

impl std::error::Error for RouteError {}

/// An address: a stack of hops, the head being where the packet goes next.
///
/// Single-hop routes are the common case. Multi-hop routes reach something nested behind a
/// gateway: the gateway receives the packet, pops its own hop, and forwards the rest.
///
/// Routes have a text form, `node@link/node@link`, used by hosts and the sandbox:
///
/// ```
/// use emergence_engine::PacketRoute;
///
/// let route: PacketRoute = "pc-1@wifi/fileman@ipc".parse()?;
/// assert_eq!(route.node(), "pc-1");
/// assert_eq!(route.popped().map(|r| r.to_string()), Some("fileman@ipc".to_owned()));
/// # Ok::<(), emergence_engine::RouteError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PacketRoute {
    hops: Vec<Hop>,
}

impl PacketRoute {
    /// Deepest route allowed. A guard against runaway route growth, not a practical limit.
    pub const MAX_DEPTH: usize = 8;

    /// A single-hop route.
    pub fn new(node: impl Into<String>, link: impl Into<String>) -> Self {
        Self {
            hops: vec![Hop::new(node, link)],
        }
    }

    /// A route from explicit hops.
    ///
    /// # Errors
    ///
    /// Fails if `hops` is empty or longer than [`MAX_DEPTH`](Self::MAX_DEPTH).
    pub fn from_hops(hops: Vec<Hop>) -> Result<Self, RouteError> {
        match hops.len() {
            0 => Err(RouteError::Empty),
            n if n > Self::MAX_DEPTH => Err(RouteError::TooDeep),
            _ => Ok(Self { hops }),
        }
    }

    /// The node the packet is going to next.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.hops[0].node
    }

    /// The link the packet should arrive on next.
    #[must_use]
    pub fn link(&self) -> &str {
        &self.hops[0].link
    }

    /// Every hop, head first.
    #[must_use]
    pub fn hops(&self) -> &[Hop] {
        &self.hops
    }

    /// Number of hops. Always at least 1.
    #[must_use]
    #[allow(clippy::len_without_is_empty)] // A route is never empty.
    pub fn len(&self) -> usize {
        self.hops.len()
    }

    /// The route with its head removed, or `None` if the head is the last hop.
    #[must_use]
    pub fn popped(&self) -> Option<Self> {
        (self.hops.len() > 1).then(|| Self {
            hops: self.hops[1..].to_vec(),
        })
    }

    /// The route with `hop` added in front.
    ///
    /// # Errors
    ///
    /// Fails if the result would be deeper than [`MAX_DEPTH`](Self::MAX_DEPTH).
    pub fn prepended(&self, hop: Hop) -> Result<Self, RouteError> {
        let mut hops = Vec::with_capacity(self.hops.len() + 1);
        hops.push(hop);
        hops.extend_from_slice(&self.hops);
        Self::from_hops(hops)
    }
}

impl fmt::Display for PacketRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, hop) in self.hops.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            write!(f, "{}@{}", hop.node, hop.link)?;
        }
        Ok(())
    }
}

impl FromStr for PacketRoute {
    type Err = RouteError;

    /// Parses `node@link/node@link`. A hop without `@link` arrives on any link (`?`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hops = s
            .split('/')
            .map(|part| {
                let part = part.trim();
                let (node, link) = part.split_once('@').unwrap_or((part, names::ANY_LINK));
                if node.is_empty() || link.is_empty() {
                    Err(RouteError::BadHop(part.to_owned()))
                } else {
                    Ok(Hop::new(node, link))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_hops(hops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_round_trip() -> Result<(), RouteError> {
        let route: PacketRoute = "pc@wifi/app@ipc".parse()?;
        assert_eq!(route.len(), 2);
        assert_eq!(route.to_string(), "pc@wifi/app@ipc");
        Ok(())
    }

    #[test]
    fn hop_without_link_means_any_link() -> Result<(), RouteError> {
        let route: PacketRoute = "pc".parse()?;
        assert_eq!(route.link(), names::ANY_LINK);
        Ok(())
    }

    #[test]
    fn rejects_bad_text() {
        assert_eq!(
            "@wifi".parse::<PacketRoute>(),
            Err(RouteError::BadHop("@wifi".into()))
        );
        assert_eq!(
            "pc@".parse::<PacketRoute>(),
            Err(RouteError::BadHop("pc@".into()))
        );
        let deep = ["a@b"; PacketRoute::MAX_DEPTH + 1].join("/");
        assert_eq!(deep.parse::<PacketRoute>(), Err(RouteError::TooDeep));
    }

    #[test]
    fn pop_stops_at_the_last_hop() -> Result<(), RouteError> {
        let route: PacketRoute = "a@x/b@y".parse()?;
        let rest = route.popped();
        assert_eq!(rest.as_ref().map(PacketRoute::node), Some("b"));
        assert_eq!(rest.and_then(|r| r.popped()), None);
        Ok(())
    }

    #[test]
    fn prepend_respects_max_depth() -> Result<(), RouteError> {
        let route = PacketRoute::new("a", "x").prepended(Hop::new("gw", "wifi"))?;
        assert_eq!(route.to_string(), "gw@wifi/a@x");
        let full = PacketRoute::from_hops(vec![Hop::new("n", "l"); PacketRoute::MAX_DEPTH])?;
        assert_eq!(full.prepended(Hop::new("m", "l")), Err(RouteError::TooDeep));
        Ok(())
    }
}
