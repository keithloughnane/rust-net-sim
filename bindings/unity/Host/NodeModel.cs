#nullable enable
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.CompilerServices;

namespace Emergence.Host
{
    /// <summary>
    /// A node whose behaviour lives in C#. Emergence holds its place in the network and carries its
    /// packets (it is an Emergence host node); this class applies its accept rules and reacts.
    /// </summary>
    public class NodeModel
    {
        private const string Tag = "NET";

        public readonly string ID;

        // The node's place in the network. Emergence holds the structure; these are read-only views
        // of it, changed only by HostNetwork, in the same step that changes Emergence (see
        // HostNetwork.Connect / SubscribeToLink / Disconnect). HostNetwork.VerifyStructure checks
        // they agree.
        private readonly List<NodeModel> _internalNodes = new List<NodeModel>();
        private readonly List<LinkModel> _internalLinks = new List<LinkModel>();
        private readonly List<LinkModel> _links = new List<LinkModel>();
        private readonly List<string> _linkIds = new List<string>();

        /// <summary>The nodes nested directly inside this one.</summary>
        public IReadOnlyList<NodeModel> InternalNodes => _internalNodes;

        /// <summary>The links this node owns (its internal links, such as a computer's ipc bus).</summary>
        public IReadOnlyList<LinkModel> InternalLinks => _internalLinks;

        /// <summary>The links this node is on.</summary>
        public IReadOnlyList<LinkModel> Links => _links;

        /// <summary>The IDs of the links this node is on.</summary>
        public IReadOnlyList<string> ExternalLinkIds => _linkIds;

        /// <summary>The node this one is nested in, if any.</summary>
        public NodeModel? Parent { get; private set; }

        private readonly Broadcaster<Packet> _packetsIntoNode = new Broadcaster<Packet>();
        private readonly Broadcaster<Packet> _packetsOuttoLink = new Broadcaster<Packet>();

        /// <summary>
        /// Every packet that reaches this node, before its accept rules. Watch only: packets come
        /// from the network (see <see cref="HostNetwork"/>), or from <see cref="PushDirect"/>.
        /// </summary>
        public IObservable<Packet> PacketsIntoNode => _packetsIntoNode;

        /// <summary>Every packet this node puts on its links. Watch only: send with <see cref="Send(Packet)"/>.</summary>
        public IObservable<Packet> PacketsOuttoLink => _packetsOuttoLink;

        private IDisposable? _receiving;
        private bool _started;

        public NodeModel(string id)
        {
            ID = id;
        }

        // ---- Structure: for HostNetwork only ------------------------------------------------------

        internal void NestChild(NodeModel child)
        {
            if (!_internalNodes.Contains(child)) _internalNodes.Add(child);
            child.Parent = this;
        }

        internal void UnnestChild(NodeModel child)
        {
            _internalNodes.Remove(child);
            if (child.Parent == this) child.Parent = null;
        }

        internal void OwnLink(LinkModel link)
        {
            if (!_internalLinks.Contains(link)) _internalLinks.Add(link);
        }

        internal void JoinLink(LinkModel link)
        {
            if (_links.Contains(link)) return;
            _links.Add(link);
            _linkIds.Add(link.ID);
        }

        internal void LeaveLink(LinkModel link)
        {
            if (_links.Remove(link)) _linkIds.Remove(link.ID);
        }

        // ---- Life -----------------------------------------------------------------------------------

        /// <summary>Starts this node and the nodes inside it: it begins to receive, then sets itself up.</summary>
        public void Start()
        {
            if (_started) return;

            HostLog.Debug(Tag, $"Starting {ID}");
            foreach (var child in InternalNodes) child.Start();

            _receiving?.Dispose();
            _receiving = _packetsIntoNode.Subscribe(new Watcher<Packet>(packet =>
            {
                if (ReceiveWhere(packet)) OnReceived(packet);
            }));
            _started = true;

            Setup();
        }

        protected virtual void Setup()
        {
        }

        public virtual void OnConnected()
        {
        }

        /// <summary>Puts anything this node has waiting to send on its links now.</summary>
        public virtual void FlushOutgoingPackets()
        {
        }

        public virtual void Dispose()
        {
            HostNetwork.Forget(this);
            _receiving?.Dispose();
            _internalLinks.Clear();
            _internalNodes.Clear();
            _packetsIntoNode.Dispose();
            _packetsOuttoLink.Dispose();
        }

        // ---- Traffic --------------------------------------------------------------------------------

        /// <summary>Puts <paramref name="packet"/> on this node's links, as if the node sent it.</summary>
        public void Send(Packet packet) => _packetsOuttoLink.OnNext(packet);

        public void Send(PacketRoute from, PacketRoute to, object? data, bool dontEcho = false) =>
            _packetsOuttoLink.OnNext(new Packet(from, to, data, dontEcho));

        /// <summary>
        /// Hands <paramref name="packet"/> straight to this node, now, on the calling thread: no link
        /// and no tick. It still goes through the network, which counts the hop, records and
        /// measures it (see <see cref="HostNetwork.PushDirect"/>). Prefer sending over a link; use
        /// this only where something must arrive immediately.
        /// </summary>
        public void PushDirect(Packet packet, [CallerFilePath] string callerFile = "",
            [CallerLineNumber] int callerLine = 0) =>
            HostNetwork.PushDirect(this, packet, $"{Path.GetFileName(callerFile)}:{callerLine}");

        /// <summary>Hands a packet the network delivered to this node. For <see cref="HostNetwork"/> only.</summary>
        internal void Deliver(Packet packet) => _packetsIntoNode.OnNext(packet);

        /// <summary>The packets that reach this node and pass its accept rules.</summary>
        public IObservable<Packet> ObservePacketsIntoNode() => new Filtered<Packet>(PacketsIntoNode, ReceiveWhere);

        public virtual PacketRoute GetSelfRoute() => new PacketRoute(ID, Addresses.Unknown);

        /// <summary>
        /// Whether this node takes <paramref name="packet"/>: it is addressed to this node (or to
        /// everyone), or comes from a child to its parent, or leaves the node's internal bus for
        /// the outside. Never its own packets.
        /// </summary>
        protected virtual bool ReceiveWhere(Packet packet) =>
            !FromSelf(packet) &&
            (ToSelf(packet) || FromChildToParent(packet) || IsFromInternalToExternal(packet));

        private bool FromChildToParent(Packet packet) =>
            packet.To.Node() == Addresses.Parent && IsChild(packet.From.Node());

        private bool ToSelf(Packet packet) =>
            packet.To.Node() == ID || packet.To.Node() == Addresses.Broadcast;

        protected bool FromSelf(Packet packet) => packet.From.Node() == ID;

        protected bool IsFromInternalToExternal(Packet packet)
        {
            if (packet.From.Link() != Addresses.Ipc) return false;
            foreach (var destination in packet.To.All())
                if (destination.Link != Addresses.Ipc)
                    return true;
            return false;
        }

        protected bool IsChild(string node)
        {
            foreach (var n in InternalNodes)
                if (n.ID == node)
                    return true;
            return false;
        }

        // Loop protection (hop budgets, route depth) is the network's job: Emergence drops a
        // packet whose budget runs out before it ever gets here.
        protected virtual void OnReceived(Packet packet)
        {
        }

        public string GetId() => ID;

        public override string ToString() => $"Node({ID})";
    }
}
