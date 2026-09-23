using System;
using System.Text;
using Emergence.Native;

namespace Emergence
{
    /// <summary>
    /// A simulation world backed by the native library. Dispose it when finished; the finalizer
    /// is only a safety net.
    /// </summary>
    /// <remarks>Not thread-safe: use each world from one thread at a time.</remarks>
    public sealed unsafe class World : IDisposable
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

        /// <summary>Advances the simulation by one tick.</summary>
        public void Tick() => EmergenceLibrary.Check(NativeMethods.emergence_world_tick(Handle));

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
        /// makes it internal to the parent and subscribes the node to it.
        /// </summary>
        public void Connect(NodeId parent, NodeId node, LinkId link = default) =>
            EmergenceLibrary.Check(NativeMethods.emergence_network_connect(
                Handle, parent.ToNative(), node.ToNative(), link.ToNative()));

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
