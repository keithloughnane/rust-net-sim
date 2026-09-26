#nullable enable
namespace Emergence.Host
{
    /// <summary>
    /// Reserved names in routes. A route is a list of hops, each a node name and the link it is
    /// reached over; these stand for something other than a named node or link.
    /// </summary>
    public static class Addresses
    {
        /// <summary>The sender's parent (as a node), such as the computer an app runs in.</summary>
        public const string Parent = "^";

        /// <summary>Every node (as a node) or any link (as a link).</summary>
        public const string Broadcast = "*";

        /// <summary>Not known yet (as a link): a node's own address before it has sent anything.</summary>
        public const string Unknown = "?";

        /// <summary>The world's root node, which holds everything else.</summary>
        public const string Root = ".";

        /// <summary>
        /// The usual name for a device's internal bus (a computer's link to its apps and services),
        /// as the Emergence templates name it.
        /// </summary>
        public const string Ipc = "ipc";
    }
}
