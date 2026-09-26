using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;
using Emergence.Native;

namespace Emergence
{
    /// <summary>One step of a route: deliver to <c>Node</c>, arriving over <c>Link</c>.</summary>
    public readonly struct Hop
    {
        /// <summary>Node name, or a reserved name such as <c>*</c> or <c>^</c>.</summary>
        public readonly string Node;
        /// <summary>Link name, or a reserved name such as <c>*</c> or <c>?</c>.</summary>
        public readonly string Link;

        /// <summary>Creates a hop.</summary>
        public Hop(string node, string link)
        {
            Node = node ?? throw new ArgumentNullException(nameof(node));
            Link = link ?? throw new ArgumentNullException(nameof(link));
        }

        /// <inheritdoc/>
        public override string ToString() => $"{Node}@{Link}";
    }

    /// <summary>A packet handed to a host node, collected with <see cref="World.DrainHostDeliveries"/>.</summary>
    public sealed class HostDelivery
    {
        /// <summary>The host node it was delivered to.</summary>
        public NodeId Receiver { get; }
        /// <summary>The link it arrived on.</summary>
        public LinkId Link { get; }
        /// <summary>Where it came from, head first.</summary>
        public IReadOnlyList<Hop> From { get; }
        /// <summary>Where it is going, head first.</summary>
        public IReadOnlyList<Hop> To { get; }
        /// <summary>The event's kind.</summary>
        public string Kind { get; }
        /// <summary>The event's payload.</summary>
        public byte[] Data { get; }
        /// <summary>
        /// Hop budget left, with this delivery counted: loop protection. Pass it back as the
        /// <c>ttl</c> of anything that forwards this packet, so loops still run out.
        /// </summary>
        public int Ttl { get; }

        internal HostDelivery(NodeId receiver, LinkId link, IReadOnlyList<Hop> from, IReadOnlyList<Hop> to,
            string kind, byte[] data, int ttl)
        {
            Receiver = receiver;
            Link = link;
            From = from;
            To = to;
            Kind = kind;
            Data = data;
            Ttl = ttl;
        }
    }

    /// <summary>What happened to a <see cref="World.HostSend"/>.</summary>
    public enum HostSendResult
    {
        /// <summary>Queued for the next tick.</summary>
        Sent,
        /// <summary>The node is not in this world.</summary>
        UnknownNode,
        /// <summary>The link is not in this world.</summary>
        UnknownLink,
        /// <summary>The node is neither subscribed to the link nor its owner.</summary>
        NotOnLink,
        /// <summary>A route was empty or deeper than 8 hops.</summary>
        InvalidRoute,
        /// <summary>The send queue is full: the world is overloaded and the packet was not sent.</summary>
        QueueFull,
        /// <summary>The payload or the event kind is too large.</summary>
        PayloadTooLarge,
    }

    /// <summary>What happened to a <see cref="World.HostPush"/>.</summary>
    public enum HostPushResult
    {
        /// <summary>Handed over: the host delivers it now.</summary>
        Delivered,
        /// <summary>Its hop budget ran out; it was dropped (and counted).</summary>
        Expired,
        /// <summary>The node is not in this world.</summary>
        UnknownNode,
        /// <summary>A route was empty or deeper than 8 hops.</summary>
        InvalidRoute,
        /// <summary>The payload or the event kind is too large.</summary>
        PayloadTooLarge,
    }

    public sealed unsafe partial class World
    {
        /// <summary>Longest event kind allowed, in UTF-8 bytes.</summary>
        public const int MaxEventKindBytes = 64;

        /// <summary>
        /// Makes <paramref name="node"/> a host node, whose behaviour lives in the host: every packet
        /// on its links is handed over, whatever the accept rules say, to be collected with
        /// <see cref="DrainHostDeliveries"/>. Host nodes send with <see cref="HostSend"/>.
        /// </summary>
        public void SetHost(NodeId node, bool host = true) =>
            EmergenceLibrary.Check(NativeMethods.emergence_world_set_host(Handle, node.ToNative(), host));

        /// <summary>
        /// Queues a packet from <paramref name="node"/> on <paramref name="link"/> with every field
        /// chosen by the host. It is delivered on the next <see cref="Tick"/>. Hop names are taken as
        /// they are, so any text is allowed. <paramref name="ttl"/> is the hop budget (loop
        /// protection): leave it null for a fresh packet, or pass a delivered packet's
        /// <see cref="HostDelivery.Ttl"/> when forwarding it.
        /// </summary>
        public HostSendResult HostSend(NodeId node, LinkId link, IReadOnlyList<Hop> from, IReadOnlyList<Hop> to,
            string eventKind, byte[] data, int? ttl = null)
        {
            var fromJson = ToUtf8(RouteJson(from), nameof(from));
            var toJson = ToUtf8(RouteJson(to), nameof(to));
            var kind = ToUtf8(eventKind, nameof(eventKind));
            data ??= Array.Empty<byte>();
            var budget = Budget(ttl);
            EmergenceStatus status;
            fixed (byte* fromPtr = fromJson)
            fixed (byte* toPtr = toJson)
            fixed (byte* kindPtr = kind)
            fixed (byte* dataPtr = data)
            {
                status = NativeMethods.emergence_world_host_send(Handle, node.ToNative(), link.ToNative(),
                    fromPtr, toPtr, kindPtr, data.Length == 0 ? null : dataPtr, (UIntPtr)data.Length, budget);
            }
            switch (status)
            {
                case EmergenceStatus.Ok: return HostSendResult.Sent;
                case EmergenceStatus.UnknownNode: return HostSendResult.UnknownNode;
                case EmergenceStatus.UnknownLink: return HostSendResult.UnknownLink;
                case EmergenceStatus.NotOnLink: return HostSendResult.NotOnLink;
                case EmergenceStatus.InvalidRoute: return HostSendResult.InvalidRoute;
                case EmergenceStatus.QueueFull: return HostSendResult.QueueFull;
                case EmergenceStatus.PayloadTooLarge: return HostSendResult.PayloadTooLarge;
                default:
                    EmergenceLibrary.Check(status);
                    throw new EmergenceException(status.ToString(), "unexpected status");
            }
        }

        /// <summary>
        /// Hands a packet straight to host node <paramref name="node"/> now: no link, no queue, no
        /// tick. It still counts as traffic (a hop of TTL, the <c>direct_pushes</c> health counter,
        /// a <c>"pushed"</c> trace entry, the monitor). The host delivers it itself, with the hop
        /// budget left in <paramref name="remainingTtl"/>.
        /// </summary>
        public HostPushResult HostPush(NodeId node, IReadOnlyList<Hop> from, IReadOnlyList<Hop> to,
            string eventKind, byte[] data, int? ttl, out int remainingTtl)
        {
            var fromJson = ToUtf8(RouteJson(from), nameof(from));
            var toJson = ToUtf8(RouteJson(to), nameof(to));
            var kind = ToUtf8(eventKind, nameof(eventKind));
            data ??= Array.Empty<byte>();
            var budget = Budget(ttl);
            byte left = 0;
            EmergenceStatus status;
            fixed (byte* fromPtr = fromJson)
            fixed (byte* toPtr = toJson)
            fixed (byte* kindPtr = kind)
            fixed (byte* dataPtr = data)
            {
                status = NativeMethods.emergence_world_host_push(Handle, node.ToNative(), fromPtr, toPtr, kindPtr,
                    data.Length == 0 ? null : dataPtr, (UIntPtr)data.Length, budget, &left);
            }
            remainingTtl = left;
            switch (status)
            {
                case EmergenceStatus.Ok: return left == 0 ? HostPushResult.Expired : HostPushResult.Delivered;
                case EmergenceStatus.UnknownNode: return HostPushResult.UnknownNode;
                case EmergenceStatus.InvalidRoute: return HostPushResult.InvalidRoute;
                case EmergenceStatus.PayloadTooLarge: return HostPushResult.PayloadTooLarge;
                default:
                    EmergenceLibrary.Check(status);
                    throw new EmergenceException(status.ToString(), "unexpected status");
            }
        }

        /// <summary>Everything delivered to host nodes since the last call, in delivery order.</summary>
        public List<HostDelivery> DrainHostDeliveries()
        {
            byte* json;
            EmergenceLibrary.Check(NativeMethods.emergence_world_drain_host_deliveries_json(Handle, &json));
            string text;
            try
            {
                text = EmergenceLibrary.FromUtf8(json);
            }
            finally
            {
                NativeMethods.emergence_string_free(json);
            }
            return HostDeliveryReader.Read(text);
        }

        /// <summary>A hop budget for the native call: 0 asks for a fresh packet's.</summary>
        private static byte Budget(int? ttl) => ttl == null ? (byte)0 : (byte)Math.Max(1, Math.Min(255, ttl.Value));

        private static string RouteJson(IReadOnlyList<Hop> route)
        {
            if (route == null) throw new ArgumentNullException(nameof(route));
            var sb = new StringBuilder("[");
            for (var i = 0; i < route.Count; i++)
            {
                if (i > 0) sb.Append(',');
                sb.Append('[');
                JsonString(sb, route[i].Node);
                sb.Append(',');
                JsonString(sb, route[i].Link);
                sb.Append(']');
            }
            return sb.Append(']').ToString();
        }

        private static void JsonString(StringBuilder sb, string value)
        {
            sb.Append('"');
            foreach (var c in value ?? "")
            {
                switch (c)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    default:
                        if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                        else sb.Append(c);
                        break;
                }
            }
            sb.Append('"');
        }
    }

    /// <summary>Reads the fixed JSON shape of <c>emergence_world_drain_host_deliveries_json</c>.</summary>
    internal sealed class HostDeliveryReader
    {
        private readonly string _s;
        private int _i;

        private HostDeliveryReader(string s) => _s = s;

        internal static List<HostDelivery> Read(string json)
        {
            var reader = new HostDeliveryReader(json);
            var result = new List<HostDelivery>();
            reader.Expect('{');
            while (!reader.TryConsume('}'))
            {
                var key = reader.String();
                reader.Expect(':');
                if (key == "deliveries")
                {
                    reader.Expect('[');
                    while (!reader.TryConsume(']'))
                    {
                        result.Add(reader.Delivery());
                        reader.TryConsume(',');
                    }
                }
                else
                {
                    reader.Skip();
                }
                reader.TryConsume(',');
            }
            return result;
        }

        private HostDelivery Delivery()
        {
            ulong receiver = 0, link = 0;
            int ttl = 0;
            IReadOnlyList<Hop> from = Array.Empty<Hop>(), to = Array.Empty<Hop>();
            string kind = "";
            byte[] data = Array.Empty<byte>();
            Expect('{');
            while (!TryConsume('}'))
            {
                var key = String();
                Expect(':');
                switch (key)
                {
                    case "receiver": receiver = Number(); break;
                    case "link": link = Number(); break;
                    case "ttl": ttl = (int)Number(); break;
                    case "from": from = Route(); break;
                    case "to": to = Route(); break;
                    case "kind": kind = String(); break;
                    case "data": data = Hex(String()); break;
                    default: Skip(); break;
                }
                TryConsume(',');
            }
            return new HostDelivery(new NodeId(receiver), new LinkId(link), from, to, kind, data, ttl);
        }

        private List<Hop> Route()
        {
            var hops = new List<Hop>();
            Expect('[');
            while (!TryConsume(']'))
            {
                Expect('[');
                var node = String();
                Expect(',');
                var link = String();
                Expect(']');
                hops.Add(new Hop(node, link));
                TryConsume(',');
            }
            return hops;
        }

        private static byte[] Hex(string hex)
        {
            var bytes = new byte[hex.Length / 2];
            for (var i = 0; i < bytes.Length; i++)
                bytes[i] = byte.Parse(hex.Substring(2 * i, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture);
            return bytes;
        }

        private void SkipSpace()
        {
            while (_i < _s.Length && char.IsWhiteSpace(_s[_i])) _i++;
        }

        private void Expect(char c)
        {
            if (!TryConsume(c)) throw new FormatException($"expected '{c}' at {_i} in host deliveries JSON");
        }

        private bool TryConsume(char c)
        {
            SkipSpace();
            if (_i < _s.Length && _s[_i] == c)
            {
                _i++;
                return true;
            }
            return false;
        }

        private ulong Number()
        {
            SkipSpace();
            var start = _i;
            while (_i < _s.Length && char.IsDigit(_s[_i])) _i++;
            return ulong.Parse(_s.Substring(start, _i - start), CultureInfo.InvariantCulture);
        }

        private string String()
        {
            Expect('"');
            var sb = new StringBuilder();
            while (_i < _s.Length && _s[_i] != '"')
            {
                var c = _s[_i++];
                if (c != '\\')
                {
                    sb.Append(c);
                    continue;
                }
                var e = _s[_i++];
                switch (e)
                {
                    case 'n': sb.Append('\n'); break;
                    case 't': sb.Append('\t'); break;
                    case 'r': sb.Append('\r'); break;
                    case 'b': sb.Append('\b'); break;
                    case 'f': sb.Append('\f'); break;
                    case 'u':
                        sb.Append((char)int.Parse(_s.Substring(_i, 4), NumberStyles.HexNumber, CultureInfo.InvariantCulture));
                        _i += 4;
                        break;
                    default: sb.Append(e); break;
                }
            }
            _i++;
            return sb.ToString();
        }

        /// <summary>Skips any JSON value (for fields added in later formats).</summary>
        private void Skip()
        {
            SkipSpace();
            var c = _s[_i];
            if (c == '"')
            {
                String();
                return;
            }
            if (c == '{' || c == '[')
            {
                var close = c == '{' ? '}' : ']';
                _i++;
                while (!TryConsume(close))
                {
                    Skip();
                    TryConsume(':');
                    TryConsume(',');
                }
                return;
            }
            while (_i < _s.Length && ",}]".IndexOf(_s[_i]) < 0) _i++;
        }
    }
}
