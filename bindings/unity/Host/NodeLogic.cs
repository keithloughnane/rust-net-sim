#nullable enable
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;

namespace Emergence.Host
{
    /// <summary>
    /// A node's behaviour, kept apart from the node (a <see cref="LogicNode"/>). What it sends
    /// waits in an outbox until the next network tick; that makes sending safe from any thread.
    /// </summary>
    public abstract class NodeLogic
    {
        public bool IsPoweredOn { get; set; } = true;

        private readonly ConcurrentQueue<Packet> _outgoingPackets = new ConcurrentQueue<Packet>();

        /// <summary>Called for each packet the node takes (see <see cref="ForceReceiveWhere"/>).</summary>
        public abstract void OnReceived(Packet packet,
            PacketRoute selfRoute,
            LogicNode thisNode,
            IReadOnlyList<NodeModel> internalNodes,
            IReadOnlyList<LinkModel> internalLinks,
            IReadOnlyList<string> externalLinkIds);

        /// <summary>Takes packets the node's own accept rules would not, such as a sniffer's.</summary>
        public virtual bool ForceReceiveWhere(string id, Packet packet) => false;

        protected virtual void Send(PacketRoute from, PacketRoute to, object? data, bool dontEcho = false) =>
            _outgoingPackets.Enqueue(new Packet(from, to, data, dontEcho));

        /// <summary>
        /// Send a new packet that carries on from <paramref name="origin"/>: it keeps the origin's
        /// loop protection, so a relay loop still runs out. Use this when forwarding/rebroadcasting.
        /// </summary>
        protected void SendFrom(Packet origin, PacketRoute from, PacketRoute to, object? data,
            bool dontEcho = false) =>
            _outgoingPackets.Enqueue(new Packet(from, to, data, dontEcho) { HopBudget = origin.HopBudget });

        protected void Send(Packet p) => _outgoingPackets.Enqueue(p);

        /// <summary>Hands everything waiting in the outbox to <paramref name="send"/>.</summary>
        public void DrainOutgoingPackets(Action<Packet> send)
        {
            while (_outgoingPackets.TryDequeue(out var p)) send(p);
        }

        /// <summary>Called once, when the node starts.</summary>
        public virtual void Setup(
            NodeModel thisNode,
            IReadOnlyList<NodeModel> internalNodes,
            IReadOnlyList<LinkModel> internalLinks)
        {
        }
    }
}
