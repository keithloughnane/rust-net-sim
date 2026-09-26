#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;

namespace Emergence.Host
{
    /// <summary>A link between nodes. Emergence carries its traffic; this class decides what it carries.</summary>
    public class LinkModel
    {
        /// <summary>
        /// Each member's subscription that puts its outgoing packets on this link. Changed only by
        /// <see cref="HostNetwork"/>.
        /// </summary>
        internal readonly Dictionary<NodeModel, IDisposable> ConnectedNodeSubscriptions =
            new Dictionary<NodeModel, IDisposable>();

        /// <summary>The nodes on this link.</summary>
        public IReadOnlyCollection<NodeModel> Members => ConnectedNodeSubscriptions.Keys;

        public readonly string ID;
        private readonly bool _strict;
        private readonly bool _allowEverything;
        private readonly Broadcaster<Packet> _packets = new Broadcaster<Packet>();

        /// <summary>Every packet this link carries, as it is put on the link.</summary>
        public IObservable<Packet> ObservePackets() => _packets;

        /// <param name="id">The link's name, as routes address it.</param>
        /// <param name="allowEverything">Carry every packet, whatever link it is addressed to.</param>
        /// <param name="strict">Refuse packets addressed to any link (<c>*</c>).</param>
        public LinkModel(string id, bool allowEverything = false, bool strict = false)
        {
            ID = id;
            _strict = strict;
            _allowEverything = allowEverything;
        }

        /// <summary>
        /// A packet <paramref name="sender"/> put on this link. If the link carries it, it reaches
        /// the link's other nodes on the next network tick (see <see cref="HostNetwork"/>).
        /// </summary>
        public void Transmit(NodeModel sender, Packet p)
        {
            if (!ReceiveWhere(p)) return;
            _packets.OnNext(p);
            HostNetwork.Transmit(sender, this, p);
        }

        protected virtual bool ReceiveWhere(Packet p)
        {
            if (_allowEverything) return true;
            return p.To.Link() == ID || (p.To.Link() == Addresses.Broadcast && !_strict);
        }

        public override string ToString() =>
            $"LinkModel: ({ID}) nodes:{string.Join(", ", Members.Select(n => n.ID))}";
    }

    /// <summary>
    /// A link that carries everything: a short-range link between whatever is in range. It echoes
    /// back to the sender too, which filters out its own packets.
    /// </summary>
    public class RangedLinkModel : LinkModel
    {
        public RangedLinkModel(string id) : base(id)
        {
        }

        protected override bool ReceiveWhere(Packet p) => true;
    }
}
