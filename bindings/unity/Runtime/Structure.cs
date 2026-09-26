using System;
using Emergence.Native;

namespace Emergence
{
    public sealed unsafe partial class World
    {
        /// <summary>The node <paramref name="node"/> is nested in, or <c>default</c> for the root or a detached node.</summary>
        public NodeId Parent(NodeId node)
        {
            EmergenceNodeId parent;
            EmergenceLibrary.Check(NativeMethods.emergence_network_parent(Handle, node.ToNative(), &parent));
            return new NodeId(parent);
        }

        /// <summary>The nodes nested directly inside <paramref name="node"/>, in the order they were added.</summary>
        public NodeId[] Children(NodeId node) =>
            ReadNodes((buffer, capacity, length) =>
                NativeMethods.emergence_network_children(Handle, node.ToNative(), buffer, capacity, length));

        /// <summary>The links <paramref name="node"/> owns: its internal links, such as a computer's <c>ipc</c> bus.</summary>
        public LinkId[] InternalLinks(NodeId node) =>
            ReadLinks((buffer, capacity, length) =>
                NativeMethods.emergence_network_internal_links(Handle, node.ToNative(), buffer, capacity, length));

        /// <summary>The links <paramref name="node"/> is subscribed to.</summary>
        public LinkId[] Subscriptions(NodeId node) =>
            ReadLinks((buffer, capacity, length) =>
                NativeMethods.emergence_network_subscriptions(Handle, node.ToNative(), buffer, capacity, length));

        /// <summary>The nodes subscribed to <paramref name="link"/>.</summary>
        public NodeId[] Subscribers(LinkId link) =>
            ReadNodes((buffer, capacity, length) =>
                NativeMethods.emergence_network_subscribers(Handle, link.ToNative(), buffer, capacity, length));

        /// <summary>The name <paramref name="node"/> was created with.</summary>
        public string NodeName(NodeId node) =>
            TakeString((world, name) => NativeMethods.emergence_network_node_name(world, node.ToNative(), name));

        /// <summary>The name <paramref name="link"/> was created with.</summary>
        public string LinkName(LinkId link) =>
            TakeString((world, name) => NativeMethods.emergence_network_link_name(world, link.ToNative(), name));

        private delegate EmergenceStatus NodeQuery(EmergenceNodeId* buffer, UIntPtr capacity, UIntPtr* length);

        private delegate EmergenceStatus LinkQuery(EmergenceLinkId* buffer, UIntPtr capacity, UIntPtr* length);

        private static NodeId[] ReadNodes(NodeQuery query)
        {
            var buffer = new EmergenceNodeId[16];
            while (true)
            {
                UIntPtr length;
                fixed (EmergenceNodeId* ptr = buffer)
                {
                    EmergenceLibrary.Check(query(ptr, (UIntPtr)buffer.Length, &length));
                }
                var count = (int)length;
                if (count > buffer.Length)
                {
                    buffer = new EmergenceNodeId[count];
                    continue;
                }
                var result = new NodeId[count];
                for (var i = 0; i < count; i++) result[i] = new NodeId(buffer[i]);
                return result;
            }
        }

        private static LinkId[] ReadLinks(LinkQuery query)
        {
            var buffer = new EmergenceLinkId[16];
            while (true)
            {
                UIntPtr length;
                fixed (EmergenceLinkId* ptr = buffer)
                {
                    EmergenceLibrary.Check(query(ptr, (UIntPtr)buffer.Length, &length));
                }
                var count = (int)length;
                if (count > buffer.Length)
                {
                    buffer = new EmergenceLinkId[count];
                    continue;
                }
                var result = new LinkId[count];
                for (var i = 0; i < count; i++) result[i] = new LinkId(buffer[i]);
                return result;
            }
        }
    }
}
