#nullable enable
using System;
using System.Collections.Generic;

namespace Emergence.Host
{
    public static partial class HostNetwork
    {
        /// <summary>Longest a packet's C# original is kept waiting for delivery, in ticks.</summary>
        private const long InFlightTicks = 200;

        private static readonly Dictionary<long, (Packet Packet, long Tick)> InFlight =
            new Dictionary<long, (Packet, long)>();

        private static readonly HashSet<LogicNode> LogicNodes = new HashSet<LogicNode>();
        private static readonly List<LogicNode> LogicNodesSnapshot = new List<LogicNode>();

        private static long _nextHandle;
        private static long _tick;
        private static long _lastPurge;
        private static long _lastFuseLog;

        /// <summary>Ticks so far.</summary>
        public static long TickCount
        {
            get
            {
                lock (Gate)
                {
                    return _tick;
                }
            }
        }

        // ---- Sending ------------------------------------------------------------------------------

        /// <summary>
        /// Puts <paramref name="packet"/>, sent by <paramref name="sender"/>, on <paramref name="link"/>.
        /// It reaches the link's other nodes on the next tick.
        /// </summary>
        internal static void Transmit(NodeModel sender, LinkModel link, Packet packet)
        {
            lock (Gate)
            {
                if (_reportedMismatch) return; // Already explained once.
                try
                {
                    var handle = ++_nextHandle;
                    InFlight[handle] = (packet, _tick);
                    var result = World.HostSend(NodeIdOf(sender), LinkIdOf(link),
                        Hops(packet.From), Hops(packet.To), Kind(packet.Data),
                        BitConverter.GetBytes(handle), Budget(packet));
                    if (result != HostSendResult.Sent)
                    {
                        InFlight.Remove(handle);
                        if (result == HostSendResult.NotOnLink) return; // It has just left the link.
                        HostLog.Error(Tag, $"{sender.ID} could not send on {link.ID} ({result}): {packet}");
                    }
                }
                catch (Exception e)
                {
                    HostLog.Error(Tag, $"{sender.ID} could not send on {link.ID}: {e}");
                }
            }
        }

        /// <summary>The hop budget to send with: the packet's own if it carries one on, else a fresh one.</summary>
        private static int? Budget(Packet packet) => packet.HopBudget > 0 ? packet.HopBudget : (int?)null;

        private static List<Hop> Hops(PacketRoute route) => new List<Hop>(route.All());

        private static PacketRoute Route(IReadOnlyList<Hop> hops) => new PacketRoute(new List<Hop>(hops));

        /// <summary>The event kind Emergence shows for a payload: its type name.</summary>
        private static string Kind(object? data)
        {
            var name = data?.GetType().Name ?? "null";
            return name.Length <= World.MaxEventKindBytes ? name : name.Substring(0, World.MaxEventKindBytes);
        }

        // ---- Direct pushes --------------------------------------------------------------------

        private const string DirectTag = "NET-DIRECT";

        /// <summary>Direct pushes so far, by call site (file:line).</summary>
        private static readonly Dictionary<string, long> DirectPushes = new Dictionary<string, long>();
        private static readonly Dictionary<string, long> DirectPushesReported = new Dictionary<string, long>();
        private static long _lastDirectReport;

        /// <summary>
        /// Hands <paramref name="packet"/> straight to <paramref name="node"/> now, on the calling
        /// thread. It goes through Emergence (World.HostPush), which spends a hop of its TTL, counts
        /// it and lets the monitor see it; it is logged here, and counted by call site
        /// (<see cref="DirectPushCounts"/>, with a summary in the log every 30 seconds).
        /// </summary>
        internal static void PushDirect(NodeModel node, Packet packet, string site)
        {
            HostPushResult result;
            int left;
            var kind = Kind(packet.Data);
            lock (Gate)
            {
                if (_reportedMismatch) return; // Already explained once.
                try
                {
                    result = World.HostPush(NodeIdOf(node), Hops(packet.From), Hops(packet.To), kind, null,
                        Budget(packet), out left);
                }
                catch (Exception e)
                {
                    HostLog.Error(DirectTag, $"{site} -> {node.ID}: {e}");
                    return;
                }
                DirectPushes.TryGetValue(site, out var count);
                DirectPushes[site] = count + 1;
            }
            HostLog.Debug(DirectTag, $"{site} -> {node.ID}: {kind}");
            switch (result)
            {
                case HostPushResult.Delivered:
                    node.Deliver(new Packet(packet.From, packet.To, packet.Data, packet.DontEcho) { HopBudget = left });
                    break;
                case HostPushResult.Expired:
                    HostLog.Warning(DirectTag, $"{site} -> {node.ID}: dropped by the network's loop protection: {packet}");
                    break;
                default:
                    HostLog.Error(DirectTag, $"{site} -> {node.ID}: not delivered ({result}): {packet}");
                    break;
            }
        }

        /// <summary>Direct pushes so far, by call site (file:line).</summary>
        public static IReadOnlyDictionary<string, long> DirectPushCounts()
        {
            lock (Gate)
            {
                return new Dictionary<string, long>(DirectPushes);
            }
        }

        /// <summary>Every 30 seconds, logs which call sites pushed directly, and how often.</summary>
        private static void ReportDirectPushes()
        {
            if (_tick - _lastDirectReport < 600) return;
            _lastDirectReport = _tick;
            var lines = new List<string>();
            foreach (var entry in DirectPushes)
            {
                DirectPushesReported.TryGetValue(entry.Key, out var before);
                if (entry.Value > before) lines.Add($"  {entry.Key}: {entry.Value - before} (total {entry.Value})");
            }
            foreach (var entry in DirectPushes) DirectPushesReported[entry.Key] = entry.Value;
            if (lines.Count > 0)
                HostLog.Debug(DirectTag, $"Direct pushes in the last 30 s:\n{string.Join("\n", lines)}");
        }

        // ---- The tick -------------------------------------------------------------------------

        /// <summary>A logic node's outbox is drained before every tick.</summary>
        internal static void Register(LogicNode node)
        {
            lock (Gate)
            {
                LogicNodes.Add(node);
            }
        }

        /// <summary>
        /// One network step: drain every logic's outbox onto the links, tick the world, and hand
        /// what arrived to its nodes. Call it from one thread only, every <see cref="TickInterval"/>.
        /// </summary>
        public static void Step()
        {
            lock (Gate)
            {
                if (_world == null) return;
                LogicNodesSnapshot.Clear();
                LogicNodesSnapshot.AddRange(LogicNodes);
            }

            foreach (var node in LogicNodesSnapshot)
            {
                try
                {
                    node.FlushOutgoingPackets();
                }
                catch (Exception e)
                {
                    HostLog.Error(Tag, $"[{node.ID}] sending: {e}");
                }
            }

            List<(NodeModel Node, Packet Packet)> arrivals;
            lock (Gate)
            {
                _tick++;
                if (World.Tick() == TickOutcome.FuseTripped && _tick - _lastFuseLog >= 20)
                {
                    _lastFuseLog = _tick;
                    HostLog.Error(Tag, $"The network fuse tripped: traffic is being held back.\n{World.FuseReportJson()}");
                }
                var deliveries = World.DrainHostDeliveries();
                arrivals = new List<(NodeModel, Packet)>(deliveries.Count);
                foreach (var delivery in deliveries)
                {
                    if (!NodesById.TryGetValue(delivery.Receiver, out var node)) continue;
                    if (Leaving.Contains((delivery.Receiver, delivery.Link))) continue;
                    var packet = Arrived(delivery);
                    if (packet != null) arrivals.Add((node, packet));
                }
                FinishLeaving();
                PurgeInFlight();
                ReportAlerts();
                if (VerifyStructureEveryTicks > 0 && _tick % VerifyStructureEveryTicks == 0) VerifyStructure();
                ReportDirectPushes();
            }

            foreach (var (node, packet) in arrivals)
            {
                try
                {
                    node.Deliver(packet);
                }
                catch (Exception e)
                {
                    HostLog.Error(Tag, $"[{node.ID}] receiving {packet.Data?.GetType().Name}: {e}");
                }
            }
        }

        /// <summary>The C# packet for a delivery: its original payload, with this copy's routes and TTL.</summary>
        private static Packet? Arrived(HostDelivery delivery)
        {
            if (delivery.Data.Length != 8) return null;
            var handle = BitConverter.ToInt64(delivery.Data, 0);
            if (!InFlight.TryGetValue(handle, out var original))
            {
                HostLog.Warning(Tag, $"A {delivery.Kind} packet arrived after its payload was forgotten; dropped.");
                return null;
            }
            var sent = original.Packet;
            return new Packet(Route(delivery.From), Route(delivery.To), sent.Data, sent.DontEcho)
            {
                HopBudget = delivery.Ttl
            };
        }

        /// <summary>Forgets payloads whose packets were delivered (or dropped) long ago.</summary>
        private static void PurgeInFlight()
        {
            if (_tick - _lastPurge < InFlightTicks) return;
            _lastPurge = _tick;
            var stale = new List<long>();
            foreach (var entry in InFlight)
                if (_tick - entry.Value.Tick > InFlightTicks)
                    stale.Add(entry.Key);
            foreach (var handle in stale) InFlight.Remove(handle);
        }
    }
}
