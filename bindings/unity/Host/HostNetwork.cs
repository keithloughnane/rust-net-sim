#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;

namespace Emergence.Host
{
    /// <summary>
    /// Runs C# nodes (<see cref="NodeModel"/>) and links (<see cref="LinkModel"/>) on an Emergence
    /// world.
    ///
    /// Emergence owns the network: the nodes and links, who is on which link, putting packets on
    /// links, the tick, hop budgets (TTL), and its safety monitor and fuse. C# keeps the behaviour:
    /// every NodeModel is an Emergence host node, so Emergence hands it every packet on the links it
    /// is on and the node applies its own accept rules and logic. Payloads stay C# objects: a
    /// packet carries a handle to its C# original, which is looked up again when it arrives.
    ///
    /// All structure changes go through this class, which changes Emergence and the nodes'
    /// read-only views of it (<see cref="NodeModel.InternalNodes"/>, InternalLinks, Links, Parent)
    /// together. <see cref="VerifyStructure"/> checks they agree.
    ///
    /// Call <see cref="Step"/> from one thread (the engine's main loop), once every
    /// <see cref="TickInterval"/>; the Emergence.Unity assembly does this in Unity. Sends from other
    /// threads are safe: they queue for the next tick.
    /// </summary>
    public static partial class HostNetwork
    {
        private const string Tag = "NET";

        /// <summary>Time between ticks.</summary>
        public static readonly TimeSpan TickInterval = TimeSpan.FromMilliseconds(50);

        private static readonly object Gate = new object();
        private static World? _world;
        private static bool _reportedMismatch;

        private static readonly Dictionary<NodeModel, NodeId> NodeIds = new Dictionary<NodeModel, NodeId>();
        private static readonly Dictionary<NodeId, NodeModel> NodesById = new Dictionary<NodeId, NodeModel>();
        private static readonly Dictionary<LinkModel, LinkId> LinkIds = new Dictionary<LinkModel, LinkId>();
        private static readonly Dictionary<LinkId, LinkModel> LinksById = new Dictionary<LinkId, LinkModel>();
        private static readonly Dictionary<string, int> NamesUsed = new Dictionary<string, int>();

        /// <summary>
        /// Nodes that left a link, still on it in Emergence until the end of the next tick so that
        /// what they sent before leaving goes out (see <see cref="Disconnect"/>).
        /// </summary>
        private static readonly HashSet<(NodeId Node, LinkId Link)> Leaving = new HashSet<(NodeId, LinkId)>();

        /// <summary>Nodes that left their parent, still nested in Emergence until the end of the next tick.</summary>
        private static readonly HashSet<(NodeId Parent, NodeId Node)> Unnesting = new HashSet<(NodeId, NodeId)>();

        /// <summary>
        /// The world, created on first use. If the native library loaded in this process is not
        /// the one the package was built for, this says so once and throws; the network stays off.
        /// </summary>
        public static World World
        {
            get
            {
                lock (Gate)
                {
                    return _world ?? CreateWorld();
                }
            }
        }

        private static World CreateWorld()
        {
            if (EmergenceLibrary.AbiVersion != EmergenceLibrary.ExpectedAbiVersion)
            {
                if (!_reportedMismatch)
                {
                    _reportedMismatch = true;
                    HostLog.Error(Tag,
                        $"The Emergence native library loaded in this process is ABI {EmergenceLibrary.AbiVersion}, " +
                        $"but the package expects ABI {EmergenceLibrary.ExpectedAbiVersion}. A native library is never " +
                        "reloaded while the process runs (in Unity: quit and reopen the project). The network is off until then.");
                }
                throw new InvalidOperationException("Emergence native library out of date: restart the process.");
            }
            var world = new World();
            world.SetTracePackets(false);
            // Generous: normal traffic should never reach these; a runaway loop will.
            world.SetLimits(maxTransmissionsPerTick: 20_000, maxDeliveriesPerTick: 400_000,
                maxPending: 100_000, maxPayloadBytes: 64);
            _world = world;
            HostLog.Debug(Tag, $"Emergence {EmergenceLibrary.Version} (ABI {EmergenceLibrary.AbiVersion}) is carrying the host's traffic");
            return world;
        }

        // ---- Structure ------------------------------------------------------------------------

        /// <summary>
        /// Nests <paramref name="node"/> inside <paramref name="parent"/> (unless it already has a
        /// parent), makes <paramref name="link"/> one of the parent's internal links, and puts the
        /// node on it. The node starts, if it had not.
        /// </summary>
        public static void Connect(NodeModel parent, NodeModel node, LinkModel? link)
        {
            if (parent == node)
            {
                HostLog.Error(Tag, $"Cannot connect node to itself: {node.ID} link: {link?.ID}");
                return;
            }
            Nest(parent, node);
            if (link != null) Own(parent, link);
            SubscribeToLink(node, link);
        }

        /// <summary>Connects each of <paramref name="nodes"/> to <paramref name="parent"/> on <paramref name="link"/>.</summary>
        public static void ConnectAll(NodeModel parent, IEnumerable<NodeModel> nodes, LinkModel link)
        {
            foreach (var node in nodes.ToList()) Connect(parent, node, link);
        }

        /// <summary>
        /// Nests <paramref name="node"/> inside <paramref name="parent"/> without putting it on a link.
        /// Does nothing if it already has a parent.
        /// </summary>
        public static void Nest(NodeModel parent, NodeModel node)
        {
            if (node.Parent == parent) return;
            if (node.Parent != null)
                HostLog.Warning(Tag, $"Node {node.ID} already has a parent, skipping add to {parent.ID}");
            else
                AddChild(parent, node);
        }

        /// <summary>Nests <paramref name="node"/> inside <paramref name="parent"/>, taking it out of any other parent first.</summary>
        public static void MoveInto(NodeModel parent, NodeModel node)
        {
            if (node.Parent == parent) return;
            if (node.Parent != null) RemoveChild(node.Parent, node);
            AddChild(parent, node);
        }

        /// <summary>Makes <paramref name="link"/> one of <paramref name="owner"/>'s internal links.</summary>
        public static void Own(NodeModel owner, LinkModel link)
        {
            if (owner.InternalLinks.Contains(link)) return;
            lock (Gate)
            {
                owner.OwnLink(link);
                if (_reportedMismatch) return; // Already explained once.
                try
                {
                    World.AddInternalLink(NodeIdOf(owner), LinkIdOf(link));
                }
                catch (Exception e)
                {
                    HostLog.Warning(Tag, $"{owner.ID} could not own link {link.ID}: {e.Message}");
                }
            }
        }

        /// <summary>
        /// Starts <paramref name="node"/> (if it had not) and puts it on <paramref name="link"/>,
        /// without changing its parent. Use <see cref="Connect"/> to nest it as well.
        /// </summary>
        public static void SubscribeToLink(NodeModel node, LinkModel? link)
        {
            HostLog.Debug(Tag, $"Connecting node {node.ID} to link {link?.ID}");
            node.Start();
            if (link == null || link.ConnectedNodeSubscriptions.ContainsKey(node)) return;
            try
            {
                Wire(node, link);
                AddSubscription(node, link);
                node.OnConnected();
            }
            catch (Exception e)
            {
                HostLog.Error(Tag, $"({node.ID}) : {e}");
            }
        }

        /// <summary>
        /// Takes <paramref name="node"/> off <paramref name="link"/> and out of
        /// <paramref name="parent"/>, the reverse of <see cref="Connect"/>. What it sent before
        /// leaving still goes out on the next tick.
        /// </summary>
        public static void Disconnect(NodeModel parent, NodeModel node, LinkModel link)
        {
            node.FlushOutgoingPackets();
            if (link.ConnectedNodeSubscriptions.TryGetValue(node, out var sending)) sending.Dispose();
            link.ConnectedNodeSubscriptions.Remove(node);
            Leave(parent, node, link);
        }

        /// <summary>Puts what <paramref name="node"/> sends on <paramref name="link"/>.</summary>
        private static void Wire(NodeModel node, LinkModel link)
        {
            var sending = node.PacketsOuttoLink.Subscribe(new Watcher<Packet>(p => link.Transmit(node, p)));
            link.ConnectedNodeSubscriptions.Add(node, sending);
        }

        // The primitive changes: each changes Emergence and the C# view together.

        private static void AddChild(NodeModel parent, NodeModel node)
        {
            lock (Gate)
            {
                parent.NestChild(node);
                if (_reportedMismatch) return;
                try
                {
                    var (parentId, nodeId) = (NodeIdOf(parent), NodeIdOf(node));
                    Unnesting.Remove((parentId, nodeId)); // Back before it had finished leaving.
                    var result = World.Connect(parentId, nodeId);
                    if (!IsConnected(result))
                        HostLog.Warning(Tag, $"{node.ID} could not be nested in {parent.ID}: {result}");
                }
                catch (Exception e)
                {
                    HostLog.Error(Tag, $"Nest {parent.ID} -> {node.ID} failed: {e}");
                }
            }
        }

        private static bool IsConnected(ConnectResult result) =>
            result.Match(
                connected: () => true,
                nameConflict: () => false,
                alreadyHasParent: () => true, // The C# side keeps the first parent too.
                wouldCreateCycle: () => false,
                isRoot: () => false,
                linkOwnedElsewhere: () => false,
                notFound: () => false);

        /// <summary>Takes <paramref name="node"/> out of <paramref name="parent"/> at once, to move it somewhere else.</summary>
        private static void RemoveChild(NodeModel parent, NodeModel node)
        {
            lock (Gate)
            {
                parent.UnnestChild(node);
                if (!NodeIds.TryGetValue(node, out var id) || !NodeIds.TryGetValue(parent, out var parentId)) return;
                Unnesting.Remove((parentId, id));
                try
                {
                    if (World.Parent(id) == parentId) World.Disconnect(parentId, id);
                }
                catch (Exception e)
                {
                    HostLog.Debug(Tag, $"Unnest {parent.ID} -> {node.ID}: {e.Message}");
                }
            }
        }

        private static void AddSubscription(NodeModel node, LinkModel link)
        {
            lock (Gate)
            {
                node.JoinLink(link);
                if (_reportedMismatch) return;
                try
                {
                    var (nodeId, linkId) = (NodeIdOf(node), LinkIdOf(link));
                    Leaving.Remove((nodeId, linkId)); // Back before it had finished leaving.
                    World.Subscribe(nodeId, linkId);
                }
                catch (Exception e)
                {
                    HostLog.Error(Tag, $"{node.ID} could not join link {link.ID}: {e}");
                }
            }
        }

        /// <summary>
        /// Takes <paramref name="node"/> off <paramref name="link"/> and out of <paramref name="parent"/>.
        ///
        /// A leaving node's last packets (a closing UI's final state, say) were queued for the next
        /// tick, and Emergence drops packets from nodes that have left the link. So in Emergence the
        /// node stays where it was until the end of the next tick (leaving its parent would also
        /// take it off the parent's links at once); anything delivered to it meanwhile is ignored,
        /// as it has left as far as C# is concerned.
        /// </summary>
        private static void Leave(NodeModel parent, NodeModel node, LinkModel link)
        {
            lock (Gate)
            {
                node.LeaveLink(link);
                parent.UnnestChild(node);
                if (!NodeIds.TryGetValue(node, out var id)) return;
                if (LinkIds.TryGetValue(link, out var linkId)) Leaving.Add((id, linkId));
                if (NodeIds.TryGetValue(parent, out var parentId)) Unnesting.Add((parentId, id));
            }
        }

        /// <summary>Takes nodes that left a link during the last tick off it in Emergence too.</summary>
        private static void FinishLeaving()
        {
            foreach (var (node, link) in Leaving)
            {
                try
                {
                    World.Unsubscribe(node, link);
                }
                catch (Exception e)
                {
                    HostLog.Debug(Tag, $"Leaving a link: {e.Message}");
                }
            }
            Leaving.Clear();
            foreach (var (parent, node) in Unnesting)
            {
                try
                {
                    if (World.Parent(node) == parent) World.Disconnect(parent, node);
                }
                catch (Exception e)
                {
                    HostLog.Debug(Tag, $"Leaving a parent: {e.Message}");
                }
            }
            Unnesting.Clear();
        }

        /// <summary>Forgets a disposed node: nothing is delivered to it any more.</summary>
        internal static void Forget(NodeModel node)
        {
            lock (Gate)
            {
                if (node is LogicNode logicNode) LogicNodes.Remove(logicNode);
                if (!NodeIds.TryGetValue(node, out var id)) return;
                NodeIds.Remove(node);
                NodesById.Remove(id);
                try
                {
                    World.SetHost(id, false);
                }
                catch (Exception e)
                {
                    HostLog.Debug(Tag, $"Forget {node.ID}: {e.Message}");
                }
            }
        }

        // ---- Emergence IDs ----------------------------------------------------------------------

        /// <summary>The Emergence node behind <paramref name="node"/>, if it has one yet.</summary>
        public static bool TryGetId(NodeModel node, out NodeId id)
        {
            lock (Gate)
            {
                return NodeIds.TryGetValue(node, out id);
            }
        }

        /// <summary>The Emergence link behind <paramref name="link"/>, if it has one yet.</summary>
        public static bool TryGetId(LinkModel link, out LinkId id)
        {
            lock (Gate)
            {
                return LinkIds.TryGetValue(link, out id);
            }
        }

        /// <summary>
        /// The Emergence node for <paramref name="node"/>, creating it the first time. A node named
        /// <see cref="Addresses.Root"/> is Emergence's own root, which never listens to links.
        /// </summary>
        private static NodeId NodeIdOf(NodeModel node)
        {
            if (NodeIds.TryGetValue(node, out var id)) return id;
            if (node.ID == Addresses.Root)
            {
                id = World.Root;
            }
            else
            {
                id = World.CreateNode(UniqueName(node.ID), node.GetType().Name);
                World.SetHost(id);
            }
            NodeIds[node] = id;
            NodesById[id] = node;
            return id;
        }

        /// <summary>The Emergence link for <paramref name="link"/>, creating it the first time.</summary>
        private static LinkId LinkIdOf(LinkModel link)
        {
            if (LinkIds.TryGetValue(link, out var id)) return id;
            id = World.CreateLink(UniqueName(link.ID));
            LinkIds[link] = id;
            LinksById[id] = link;
            return id;
        }

        /// <summary>
        /// Emergence names are unique here so that every C# node and link, whatever its ID, has a
        /// valid, unambiguous name there. Addresses in packets keep the C# IDs unchanged; the
        /// names only show up in Emergence's own tools.
        /// </summary>
        private static string UniqueName(string id)
        {
            var name = new StringBuilder();
            foreach (var c in id ?? "")
            {
                if (char.IsControl(c)) continue;
                name.Append(c == '@' || c == '/' ? '_' : c);
            }
            var clean = name.ToString().Trim();
            if (clean.Length == 0 || clean == Addresses.Root || clean == Addresses.Parent ||
                clean == Addresses.Broadcast || clean == Addresses.Unknown)
                clean = "~" + clean;
            if (clean.Length > 60) clean = clean.Substring(0, 60);
            NamesUsed.TryGetValue(clean, out var used);
            NamesUsed[clean] = used + 1;
            return used == 0 ? clean : $"{clean}#{used + 1}";
        }
    }
}
