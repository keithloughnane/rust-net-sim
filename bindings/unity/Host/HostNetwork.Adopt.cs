#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;

namespace Emergence.Host
{
    public static partial class HostNetwork
    {
        // ---- Nodes Emergence built ----------------------------------------------------------------

        /// <summary>
        /// Builds a node from one of Emergence's templates (see World.BuildTemplateAt) inside
        /// <paramref name="parent"/>, on <paramref name="link"/> if given. Its nodes run their
        /// Emergence logic; to give one of them C# behaviour, <see cref="Adopt(NodeId, NodeModel)"/>
        /// it (the root is in the result).
        /// </summary>
        public static BuildAtResult BuildTemplateAt(string template, string name, NodeModel parent,
            LinkModel? link = null, string? specJson = null)
        {
            lock (Gate)
            {
                var parentId = NodeIdOf(parent);
                var result = World.BuildTemplateAt(template, name, parentId,
                    link == null ? default : LinkIdOf(link), specJson);
                // Building on a link makes the parent own it, if nothing did.
                if (link != null && World.InternalLinks(parentId).Contains(LinkIdOf(link))) parent.OwnLink(link);
                return result;
            }
        }

        /// <summary>
        /// Gives the Emergence node <paramref name="id"/> (one a template built, say) C# behaviour:
        /// from now on <paramref name="node"/> is that node. It becomes a host node, so its Emergence
        /// logic stops and every packet on its links goes to <paramref name="node"/>, and C#'s view of
        /// the structure is filled in from Emergence: its parent and children (those that are C#
        /// nodes), the links it owns and the links it is on. Links C# has not seen yet get a
        /// <see cref="LinkModel"/> named as in Emergence; call <see cref="Adopt(LinkId, LinkModel)"/>
        /// first to use your own. The node starts.
        /// </summary>
        public static void Adopt(NodeId id, NodeModel node)
        {
            lock (Gate)
            {
                if (NodeIds.ContainsKey(node))
                    throw new InvalidOperationException($"{node.ID} already has an Emergence node.");
                if (NodesById.ContainsKey(id))
                    throw new InvalidOperationException($"{id} already has a C# node.");
                World.SetHost(id);
                NodeIds[node] = id;
                NodesById[id] = node;
                Reserve(World.NodeName(id));

                var parent = World.Parent(id);
                if (parent.IsValid && NodesById.TryGetValue(parent, out var parentNode)) parentNode.NestChild(node);
                foreach (var child in World.Children(id))
                    if (NodesById.TryGetValue(child, out var childNode))
                        node.NestChild(childNode);
                foreach (var owned in World.InternalLinks(id)) node.OwnLink(LinkFor(owned));
                foreach (var on in World.Subscriptions(id))
                {
                    var link = LinkFor(on);
                    node.JoinLink(link);
                    if (!link.ConnectedNodeSubscriptions.ContainsKey(node)) Wire(node, link);
                }
            }
            node.Start();
        }

        /// <summary>Uses <paramref name="link"/> for the Emergence link <paramref name="id"/>.</summary>
        public static void Adopt(LinkId id, LinkModel link)
        {
            lock (Gate)
            {
                if (LinkIds.ContainsKey(link))
                    throw new InvalidOperationException($"{link.ID} already has an Emergence link.");
                if (LinksById.ContainsKey(id))
                    throw new InvalidOperationException($"{id} already has a C# link.");
                LinkIds[link] = id;
                LinksById[id] = link;
                Reserve(World.LinkName(id));
            }
        }

        /// <summary>The C# link for an Emergence link, made the first time it is seen.</summary>
        private static LinkModel LinkFor(LinkId id)
        {
            if (LinksById.TryGetValue(id, out var link)) return link;
            link = new LinkModel(World.LinkName(id));
            LinkIds[link] = id;
            LinksById[id] = link;
            return link;
        }

        /// <summary>Keeps <see cref="UniqueName"/> from handing out a name Emergence already uses.</summary>
        private static void Reserve(string name)
        {
            NamesUsed.TryGetValue(name, out var used);
            NamesUsed[name] = Math.Max(used, 1);
        }

        // ---- Checking the structure ----------------------------------------------------------

        /// <summary>
        /// How often <see cref="Step"/> runs <see cref="VerifyStructure"/>, in ticks; 0 for never.
        /// The Unity driver turns it on in the editor.
        /// </summary>
        public static long VerifyStructureEveryTicks { get; set; }

        private static readonly HashSet<string> ReportedDrift = new HashSet<string>();

        /// <summary>
        /// Compares every C# node's read-only view of the structure (parent, children, owned links,
        /// links it is on) with Emergence's, and logs each difference once. They are changed
        /// together, so a difference is a bug: something changed one without the other. Nodes
        /// that only Emergence knows (a template's, running Emergence logic) are left out.
        /// Returns the differences found.
        /// </summary>
        public static List<string> VerifyStructure()
        {
            lock (Gate)
            {
                var drift = new List<string>();
                if (_world == null) return drift;
                foreach (var entry in NodeIds)
                {
                    var (node, id) = (entry.Key, entry.Value);
                    if (id == World.Root) continue; // The world root holds every top level.
                    Compare(drift, node, "parent", Ids(node.Parent), Ids(World.Parent(id)));
                    Compare(drift, node, "children", node.InternalNodes.Select(IdOrNone),
                        World.Children(id).Where(NodesById.ContainsKey).Select(Named));
                    Compare(drift, node, "owned links", node.InternalLinks.Select(IdOrNone),
                        World.InternalLinks(id).Select(Named));
                    var leaving = Leaving.Where(l => l.Node == id).Select(l => Named(l.Link));
                    Compare(drift, node, "links", node.Links.Select(IdOrNone),
                        World.Subscriptions(id).Select(Named).Except(leaving));
                }
                foreach (var line in drift)
                    if (ReportedDrift.Add(line))
                        HostLog.Warning(Tag, $"C#'s view of the network differs from Emergence: {line}");
                return drift;
            }
        }

        private static IEnumerable<string> Ids(NodeModel? node) =>
            node == null ? Array.Empty<string>() : new[] { IdOrNone(node) };

        private static IEnumerable<string> Ids(NodeId id) =>
            id.IsValid && (id == World.Root || NodesById.ContainsKey(id)) ? new[] { Named(id) } : Array.Empty<string>();

        private static string IdOrNone(NodeModel node) =>
            NodeIds.ContainsKey(node) ? node.ID : $"{node.ID} (not in Emergence)";

        private static string IdOrNone(LinkModel link) =>
            LinkIds.ContainsKey(link) ? link.ID : $"{link.ID} (not in Emergence)";

        private static string Named(NodeId id) =>
            id == World.Root ? Addresses.Root : NodesById.TryGetValue(id, out var node) ? node.ID : $"{id} (not in C#)";

        private static string Named(LinkId id) =>
            LinksById.TryGetValue(id, out var link) ? link.ID : $"{id} (not in C#)";

        private static void Compare(List<string> drift, NodeModel node, string what, IEnumerable<string> host,
            IEnumerable<string> emergence)
        {
            var h = host.OrderBy(x => x, StringComparer.Ordinal).ToList();
            var e = emergence.OrderBy(x => x, StringComparer.Ordinal).ToList();
            if (!h.SequenceEqual(e))
                drift.Add($"{node.ID} {what}: C# [{string.Join(", ", h)}], Emergence [{string.Join(", ", e)}]");
        }

        // ---- Diagnostics ----------------------------------------------------------------------

        private static long _lastAlertCheck;

        /// <summary>
        /// Packet tracing is off, so the trace only holds the monitor's alerts (loops, floods,
        /// replays) and notes. Read it now and then and pass alerts on to the log.
        /// </summary>
        private static void ReportAlerts()
        {
            if (_tick - _lastAlertCheck < 40) return;
            _lastAlertCheck = _tick;
            var trace = World.DrainTraceJson();
            if (trace.Contains("\"alert\""))
                HostLog.Warning(Tag, $"The network monitor raised alerts:\n{trace}");
        }

        /// <summary>The network's load and safety counters, as JSON, for debugging.</summary>
        public static string HealthJson()
        {
            lock (Gate)
            {
                return _world?.HealthJson() ?? "{}";
            }
        }

        /// <summary>The whole Emergence network, as JSON, for debugging.</summary>
        public static string SnapshotJson()
        {
            lock (Gate)
            {
                return _world?.SnapshotJson() ?? "{}";
            }
        }
    }
}
