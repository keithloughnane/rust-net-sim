using System;
using Emergence.Native;

namespace Emergence
{
    /// <summary>Identifies a node within a <see cref="World"/>. <c>default</c> means "no node".</summary>
    public readonly struct NodeId : IEquatable<NodeId>
    {
        internal readonly ulong Raw;

        internal NodeId(ulong raw) => Raw = raw;
        internal NodeId(EmergenceNodeId id) => Raw = id.raw;
        internal EmergenceNodeId ToNative() => new EmergenceNodeId { raw = Raw };

        /// <summary>True if this refers to a node (it may still have been removed).</summary>
        public bool IsValid => Raw != 0;

        /// <inheritdoc/>
        public bool Equals(NodeId other) => Raw == other.Raw;
        /// <inheritdoc/>
        public override bool Equals(object obj) => obj is NodeId other && Equals(other);
        /// <inheritdoc/>
        public override int GetHashCode() => Raw.GetHashCode();
        /// <inheritdoc/>
        public override string ToString() => $"Node({Raw:X})";
        /// <summary>Compares two IDs.</summary>
        public static bool operator ==(NodeId a, NodeId b) => a.Equals(b);
        /// <summary>Compares two IDs.</summary>
        public static bool operator !=(NodeId a, NodeId b) => !a.Equals(b);
    }

    /// <summary>Identifies a link within a <see cref="World"/>. <c>default</c> means "no link".</summary>
    public readonly struct LinkId : IEquatable<LinkId>
    {
        internal readonly ulong Raw;

        internal LinkId(ulong raw) => Raw = raw;
        internal LinkId(EmergenceLinkId id) => Raw = id.raw;
        internal EmergenceLinkId ToNative() => new EmergenceLinkId { raw = Raw };

        /// <summary>True if this refers to a link (it may still have been removed).</summary>
        public bool IsValid => Raw != 0;

        /// <inheritdoc/>
        public bool Equals(LinkId other) => Raw == other.Raw;
        /// <inheritdoc/>
        public override bool Equals(object obj) => obj is LinkId other && Equals(other);
        /// <inheritdoc/>
        public override int GetHashCode() => Raw.GetHashCode();
        /// <inheritdoc/>
        public override string ToString() => $"Link({Raw:X})";
        /// <summary>Compares two IDs.</summary>
        public static bool operator ==(LinkId a, LinkId b) => a.Equals(b);
        /// <summary>Compares two IDs.</summary>
        public static bool operator !=(LinkId a, LinkId b) => !a.Equals(b);
    }
}
