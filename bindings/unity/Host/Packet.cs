#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;

namespace Emergence.Host
{
    /// <summary>
    /// A packet as the host sees it: routes and a payload, which stays a C# object. Emergence
    /// carries a handle to it and the host looks the payload up again when the packet arrives.
    /// </summary>
    public class Packet
    {
        public readonly object? Data;
        public readonly PacketRoute From;
        public readonly PacketRoute To;

        /// <summary>
        /// Stops an infinite replay when a transparent relay (a phone, a hub) resends the packet.
        /// The relay can't use the from address to spot its own rebroadcasts, because it keeps the
        /// original sender. TODO: find a more principled solution; this breaks if two tunnels are
        /// connected, for example.
        /// </summary>
        public readonly bool DontEcho;

        /// <summary>
        /// The network's loop protection for this packet (its remaining hop budget, as Emergence
        /// reported it), or 0 for a fresh packet. The network's business only: game code never
        /// reads it. A packet that forwards another carries it on (see
        /// <see cref="NodeLogic.SendFrom"/>), so loops still run out.
        /// </summary>
        internal int HopBudget;

        public Packet(PacketRoute from, PacketRoute to, object? data, bool dontEcho = false)
        {
            From = from;
            To = to;
            Data = data;
            DontEcho = dontEcho;
        }

        public Packet Clone() => new Packet(From, To, Data, DontEcho) { HopBudget = HopBudget };

        public override string ToString() => $"[data:{Data}] from:{From} to:{To} dontEcho:{DontEcho}]";
    }

    /// <summary>
    /// Where a packet comes from or goes to: a stack of hops, each a node and the link it is
    /// reached over. Each hop is removed as it is reached, and the packet is sent on to the next.
    /// </summary>
    public class PacketRoute
    {
        private const string Tag = "NET";

        private readonly List<Hop> _route;

        public PacketRoute(string node, string link)
        {
            _route = new List<Hop> { new Hop(node, link) };
        }

        public PacketRoute(List<Hop> route)
        {
            _route = route ?? throw new ArgumentNullException(nameof(route));
            if (_route.Count == 0) throw new ArgumentException("A route needs at least one hop.", nameof(route));
            // How deep a route may be is loop protection, and the network's business: Emergence
            // refuses routes that are too deep.
        }

        public PacketRoute(IEnumerable<Hop> route) : this(route.ToList())
        {
        }

        public override string ToString() =>
            _route.Aggregate("[", (current, hop) => current + $": {hop.Node}.{hop.Link}") + "]";

        /// <summary>The first hop's link.</summary>
        public string Link() => _route[0].Link;

        /// <summary>The first hop's node.</summary>
        public string Node() => _route[0].Node;

        /// <summary>Removes the first hop. A route keeps its last hop: popping that is logged as an error.</summary>
        public PacketRoute Pop()
        {
            if (_route.Count == 1)
                HostLog.Error(Tag, $"Tried to pop the last hop of a route: {this}");
            else
                _route.RemoveAt(0);
            return this;
        }

        /// <summary>Removes the first hop, unless it is the last one.</summary>
        public PacketRoute TryPop()
        {
            if (_route.Count != 1) _route.RemoveAt(0);
            return this;
        }

        /// <summary>The hops, first first. This is the route's own list.</summary>
        public List<Hop> All() => _route;

        public int Count() => _route.Count;

        /// <summary>Whether <paramref name="node"/> is any hop's node.</summary>
        public bool Visits(string node)
        {
            foreach (var hop in _route)
                if (hop.Node == node)
                    return true;
            return false;
        }
    }
}
