#nullable enable
using System;

namespace Emergence.Host
{
    /// <summary>A node whose behaviour is a <see cref="NodeLogic"/>, made when the node starts.</summary>
    public abstract class LogicNode : NodeModel
    {
        private const string Tag = "NET";

        protected NodeLogic? _logic;
        private Action<Packet>? _sendDelegate;

        /// <param name="id">The node's name.</param>
        /// <param name="networkInterval">
        /// Unused: every node runs on the network's tick (<see cref="HostNetwork.TickInterval"/>).
        /// Kept so existing callers compile.
        /// </param>
        protected LogicNode(string id, double networkInterval = 50) : base(id)
        {
        }

        protected abstract NodeLogic CreateLogic();

        /// <summary>Puts what the logic has waiting to send on the node's links.</summary>
        protected void CollectPackets()
        {
            if (_logic != null && _sendDelegate != null) _logic.DrainOutgoingPackets(_sendDelegate);
        }

        public override void FlushOutgoingPackets() => CollectPackets();

        [Obsolete("Reach the logic through the node's own methods where you can.")]
        public virtual NodeLogic? GetLogic() => _logic;

        protected override void Setup()
        {
            base.Setup();
            var logic = CreateLogic();
            _logic = logic;
            _sendDelegate = Send;

            // The network drains this node's outbox before every tick (HostNetwork.Step).
            HostNetwork.Register(this);

            logic.Setup(this, InternalNodes, InternalLinks);
        }

        protected override bool ReceiveWhere(Packet packet) =>
            base.ReceiveWhere(packet) || (_logic?.ForceReceiveWhere(ID, packet) ?? false);

        protected override void OnReceived(Packet packet)
        {
            var logic = _logic;
            if (logic == null || !logic.IsPoweredOn) return;
            try
            {
                logic.OnReceived(packet, GetSelfRoute(), this, InternalNodes, InternalLinks, ExternalLinkIds);
            }
            catch (Exception ex)
            {
                HostLog.Error(Tag,
                    $"{ID} ({logic.GetType().Name}) failed handling {packet.Data?.GetType().Name}: {ex}");
            }
        }
    }
}
