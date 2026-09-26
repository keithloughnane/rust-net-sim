using System;
using System.Text;
using Emergence.Native;

namespace Emergence
{
    /// <summary>How a tick ended.</summary>
    public enum TickOutcome
    {
        /// <summary>Everything due was delivered.</summary>
        Completed,
        /// <summary>A hard limit was hit and packets were held back. Pause and inspect.</summary>
        FuseTripped,
    }

    /// <summary>
    /// A simulation world backed by the native library. Dispose it when finished; the finalizer
    /// is only a safety net.
    /// </summary>
    /// <remarks>Not thread-safe: use each world from one thread at a time.</remarks>
    public sealed unsafe partial class World : IDisposable
    {
        private EmergenceWorld* _handle;

        /// <summary>Creates a world whose network contains only the root node, at tick zero.</summary>
        public World()
        {
            EmergenceLibrary.EnsureCompatible();
            EmergenceWorld* handle;
            EmergenceLibrary.Check(NativeMethods.emergence_world_create(&handle));
            _handle = handle;
        }

        /// <summary>Number of ticks run since the world was created.</summary>
        public ulong TickCount
        {
            get
            {
                ulong count;
                EmergenceLibrary.Check(NativeMethods.emergence_world_tick_count(Handle, &count));
                return count;
            }
        }

        /// <summary>
        /// Advances the simulation by one tick. Returns <see cref="TickOutcome.FuseTripped"/> if the
        /// tick hit a hard limit: the network is running away, so pause and read
        /// <see cref="FuseReportJson"/>.
        /// </summary>
        public TickOutcome Tick()
        {
            var status = NativeMethods.emergence_world_tick(Handle);
            if (status == EmergenceStatus.FuseTripped) return TickOutcome.FuseTripped;
            EmergenceLibrary.Check(status);
            return TickOutcome.Completed;
        }

        /// <summary>Sets the fuse's hard limits. 0 keeps a limit's current value.</summary>
        public void SetLimits(ulong maxTransmissionsPerTick = 0, ulong maxDeliveriesPerTick = 0,
            ulong maxPending = 0, ulong maxPayloadBytes = 0) =>
            EmergenceLibrary.Check(NativeMethods.emergence_world_set_limits(
                Handle, maxTransmissionsPerTick, maxDeliveriesPerTick, maxPending, maxPayloadBytes));

        /// <summary>
        /// Whether every packet is recorded in the trace (default on). Off is much cheaper for busy
        /// networks; alerts and notes are always recorded.
        /// </summary>
        public void SetTracePackets(bool enabled) =>
            EmergenceLibrary.Check(NativeMethods.emergence_world_set_trace_packets(Handle, enabled ? 1u : 0u));

        /// <summary>
        /// Builds a node from a template (see <see cref="EmergenceLibrary.TemplatesCatalogJson"/>),
        /// detached: attach it with <see cref="Connect"/>. If that connect then fails, the node
        /// stays built but unattached. On failure here, nothing was built.
        /// </summary>
        /// <param name="template">"computer" or "npc".</param>
        /// <param name="name">The new node's name.</param>
        /// <param name="specJson">The template's parameters as JSON, or null for defaults.</param>
        public BuildResult BuildTemplate(string template, string name, string specJson = null)
        {
            var (status, root, why) = CallBuild(template, name, specJson, null);
            return BuildResult.From(status, root, why);
        }

        /// <summary>
        /// Builds a node from a template and connects it in one step: inside
        /// <paramref name="parent"/>, on <paramref name="link"/> (default: no link). On any failure,
        /// nothing was added to the world.
        /// </summary>
        public BuildAtResult BuildTemplateAt(string template, string name, NodeId parent,
            LinkId link = default, string specJson = null)
        {
            var (status, root, why) = CallBuild(template, name, specJson, (parent, link));
            return BuildAtResult.From(status, root, why);
        }

        /// <summary>Builds a computer with the given apps and hardware tags (catalogue names).</summary>
        public BuildResult BuildComputer(string name, string[] apps = null, string[] hardware = null) =>
            BuildTemplate("computer", name, ComputerSpec(apps, hardware));

        /// <summary>Builds a computer and connects it in one step.</summary>
        public BuildAtResult BuildComputerAt(string name, NodeId parent, LinkId link = default,
            string[] apps = null, string[] hardware = null) =>
            BuildTemplateAt("computer", name, parent, link, ComputerSpec(apps, hardware));

        private static string ComputerSpec(string[] apps, string[] hardware) =>
            "{\"apps\":" + JsonArray(apps) + ",\"hardware\":" + JsonArray(hardware) + "}";

        private (EmergenceStatus, NodeId, string) CallBuild(string template, string name, string specJson,
            (NodeId parent, LinkId link)? at)
        {
            var t = ToUtf8(template, nameof(template));
            var n = ToUtf8(name, nameof(name));
            var spec = specJson == null ? null : ToUtf8(specJson, nameof(specJson));
            EmergenceNodeId id;
            EmergenceStatus status;
            fixed (byte* tPtr = t)
            fixed (byte* nPtr = n)
            fixed (byte* sPtr = spec)
            {
                status = at is var (parent, link)
                    ? NativeMethods.emergence_template_build_at(
                        Handle, tPtr, nPtr, sPtr, parent.ToNative(), link.ToNative(), &id)
                    : NativeMethods.emergence_template_build(Handle, tPtr, nPtr, sPtr, &id);
            }
            var why = status == EmergenceStatus.Ok
                ? ""
                : EmergenceLibrary.FromUtf8(NativeMethods.emergence_world_last_error(Handle));
            return (status, new NodeId(id), why);
        }

        private static string JsonArray(string[] items)
        {
            if (items == null || items.Length == 0) return "[]";
            var sb = new StringBuilder("[");
            for (var i = 0; i < items.Length; i++)
            {
                if (i > 0) sb.Append(',');
                sb.Append('"');
                foreach (var c in items[i])
                {
                    if (c == '"' || c == '\\') sb.Append('\\');
                    sb.Append(c);
                }
                sb.Append('"');
            }
            return sb.Append(']').ToString();
        }

        /// <summary>The world's load and safety counters, as JSON.</summary>
        public string HealthJson() => TakeString(NativeMethods.emergence_world_health_json);

        /// <summary>Why the fuse tripped on the last tick, as JSON, or "null" if it did not.</summary>
        public string FuseReportJson() => TakeString(NativeMethods.emergence_world_fuse_report_json);

        private delegate EmergenceStatus StringGetter(EmergenceWorld* world, byte** json);

        private string TakeString(StringGetter getter)
        {
            byte* json;
            EmergenceLibrary.Check(getter(Handle, &json));
            try
            {
                return EmergenceLibrary.FromUtf8(json);
            }
            finally
            {
                NativeMethods.emergence_string_free(json);
            }
        }

        /// <summary>The network's root node.</summary>
        public NodeId Root
        {
            get
            {
                EmergenceNodeId id;
                EmergenceLibrary.Check(NativeMethods.emergence_network_root(Handle, &id));
                return new NodeId(id);
            }
        }

        /// <summary>Creates a detached node. Attach it with <see cref="Connect"/>.</summary>
        /// <param name="name">Name used for routing.</param>
        /// <param name="kind">Free-form label such as "computer" or "app".</param>
        public NodeId CreateNode(string name, string kind)
        {
            var nameBytes = ToUtf8(name, nameof(name));
            var kindBytes = ToUtf8(kind, nameof(kind));
            EmergenceNodeId id;
            fixed (byte* namePtr = nameBytes)
            fixed (byte* kindPtr = kindBytes)
            {
                EmergenceLibrary.Check(
                    NativeMethods.emergence_network_create_node(Handle, namePtr, kindPtr, &id));
            }
            return new NodeId(id);
        }

        /// <summary>Creates a link with no owner and no subscribers.</summary>
        public LinkId CreateLink(string name)
        {
            var nameBytes = ToUtf8(name, nameof(name));
            EmergenceLinkId id;
            fixed (byte* namePtr = nameBytes)
            {
                EmergenceLibrary.Check(NativeMethods.emergence_network_create_link(Handle, namePtr, &id));
            }
            return new LinkId(id);
        }

        /// <summary>Makes <paramref name="link"/> internal to <paramref name="owner"/>.</summary>
        public void AddInternalLink(NodeId owner, LinkId link) =>
            EmergenceLibrary.Check(NativeMethods.emergence_network_add_internal_link(
                Handle, owner.ToNative(), link.ToNative()));

        /// <summary>
        /// Nests <paramref name="node"/> inside <paramref name="parent"/>. If a link is given, also
        /// makes it internal to the parent and subscribes the node to it. On failure, nothing
        /// changed.
        /// </summary>
        public ConnectResult Connect(NodeId parent, NodeId node, LinkId link = default) =>
            ConnectResult.From(NativeMethods.emergence_network_connect(
                Handle, parent.ToNative(), node.ToNative(), link.ToNative()));

        /// <summary>
        /// Deletes <paramref name="node"/>, everything inside it, and the links they own. Other
        /// nodes lose their subscriptions to those links.
        /// </summary>
        public RemoveResult RemoveNode(NodeId node) =>
            RemoveResult.From(NativeMethods.emergence_network_remove_node(Handle, node.ToNative()));

        /// <summary>Deletes <paramref name="link"/>; its subscribers and owner stay.</summary>
        public RemoveResult RemoveLink(LinkId link) =>
            RemoveResult.From(NativeMethods.emergence_network_remove_link(Handle, link.ToNative()));

        /// <summary>Attaches <paramref name="node"/> to <paramref name="link"/> without changing the hierarchy.</summary>
        public void Subscribe(NodeId node, LinkId link) =>
            EmergenceLibrary.Check(NativeMethods.emergence_network_subscribe(
                Handle, node.ToNative(), link.ToNative()));

        /// <summary>Detaches <paramref name="node"/> from <paramref name="link"/>.</summary>
        public void Unsubscribe(NodeId node, LinkId link) =>
            EmergenceLibrary.Check(NativeMethods.emergence_network_unsubscribe(
                Handle, node.ToNative(), link.ToNative()));

        /// <summary>Removes <paramref name="node"/> from <paramref name="parent"/>, the inverse of <see cref="Connect"/>.</summary>
        public void Disconnect(NodeId parent, NodeId node) =>
            EmergenceLibrary.Check(NativeMethods.emergence_network_disconnect(
                Handle, parent.ToNative(), node.ToNative()));

        /// <summary>
        /// Attaches a built-in logic to <paramref name="node"/> by name (see
        /// <see cref="EmergenceLibrary.LogicKindsJson"/>). Null, empty or "none" removes it.
        /// </summary>
        public void SetLogic(NodeId node, string kind)
        {
            var kindBytes = ToUtf8(kind ?? string.Empty, nameof(kind));
            fixed (byte* kindPtr = kindBytes)
            {
                EmergenceLibrary.Check(NativeMethods.emergence_world_set_logic(Handle, node.ToNative(), kindPtr));
            }
        }

        /// <summary>
        /// Queues an event from <paramref name="node"/>, delivered on the next <see cref="Tick"/>.
        /// </summary>
        /// <param name="node">The sending node.</param>
        /// <param name="viaLink">Name of a link the node subscribes to or owns.</param>
        /// <param name="toRoute">Destination such as "pc-1@wifi", "*@wifi" or "pc-1@wifi/fileman@ipc".</param>
        /// <param name="eventKind">What the event is, such as "ping".</param>
        /// <param name="data">Optional payload.</param>
        public void Send(NodeId node, string viaLink, string toRoute, string eventKind, byte[] data = null)
        {
            var via = ToUtf8(viaLink, nameof(viaLink));
            var to = ToUtf8(toRoute, nameof(toRoute));
            var kind = ToUtf8(eventKind, nameof(eventKind));
            data ??= Array.Empty<byte>();
            fixed (byte* viaPtr = via)
            fixed (byte* toPtr = to)
            fixed (byte* kindPtr = kind)
            fixed (byte* dataPtr = data)
            {
                EmergenceLibrary.Check(NativeMethods.emergence_world_send(
                    Handle, node.ToNative(), viaPtr, toPtr, kindPtr,
                    data.Length == 0 ? null : dataPtr, (UIntPtr)data.Length));
            }
        }

        /// <summary>
        /// Returns everything that happened since the last call as JSON, and clears it (see
        /// <c>crates/emergence-ffi/src/trace.rs</c> for the format). Call it regularly.
        /// </summary>
        public string DrainTraceJson()
        {
            byte* json;
            EmergenceLibrary.Check(NativeMethods.emergence_world_drain_trace_json(Handle, &json));
            try
            {
                return EmergenceLibrary.FromUtf8(json);
            }
            finally
            {
                NativeMethods.emergence_string_free(json);
            }
        }

        /// <summary>
        /// Returns a JSON description of the whole network (see
        /// <c>crates/emergence-ffi/src/snapshot.rs</c> for the format).
        /// </summary>
        public string SnapshotJson()
        {
            byte* json;
            EmergenceLibrary.Check(NativeMethods.emergence_network_snapshot_json(Handle, &json));
            try
            {
                return EmergenceLibrary.FromUtf8(json);
            }
            finally
            {
                NativeMethods.emergence_string_free(json);
            }
        }

        /// <summary>Releases the native world.</summary>
        public void Dispose()
        {
            Release();
            GC.SuppressFinalize(this);
        }

        ~World() => Release();

        private EmergenceWorld* Handle =>
            _handle != null ? _handle : throw new ObjectDisposedException(nameof(World));

        private void Release()
        {
            if (_handle == null) return;
            NativeMethods.emergence_world_destroy(_handle);
            _handle = null;
        }

        /// <summary>Encodes a string as NUL-terminated UTF-8 for passing to the native library.</summary>
        private static byte[] ToUtf8(string value, string paramName)
        {
            if (value == null) throw new ArgumentNullException(paramName);
            var bytes = new byte[Encoding.UTF8.GetByteCount(value) + 1];
            Encoding.UTF8.GetBytes(value, 0, value.Length, bytes, 0);
            return bytes;
        }
    }
}
