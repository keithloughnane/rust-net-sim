using System;
using Emergence.Native;

namespace Emergence
{
    // Outcomes the caller is expected to handle come back as sealed result types: a closed set of
    // cases, some carrying data, like a Kotlin sealed interface. Each has a Match method that takes
    // one handler per case, so the compiler makes sure every case is handled. (C# cannot check a
    // switch over classes for exhaustiveness; Match is how that is enforced.) Bugs in the calling
    // code, such as using a disposed world, still throw.

    /// <summary>The result of building a node from a template.</summary>
    public abstract class BuildResult
    {
        private BuildResult() { }

        /// <summary>Built, detached. Attach it with <see cref="World.Connect"/>.</summary>
        public sealed class Built : BuildResult
        {
            /// <summary>The new node's root.</summary>
            public NodeId Root { get; }
            internal Built(NodeId root) => Root = root;
        }

        /// <summary>The name is empty, reserved, or contains route syntax.</summary>
        public sealed class InvalidName : BuildResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal InvalidName(string reason) => Reason = reason;
        }

        /// <summary>The spec is not valid for the template.</summary>
        public sealed class InvalidSpec : BuildResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal InvalidSpec(string reason) => Reason = reason;
        }

        /// <summary>No template has that name.</summary>
        public sealed class UnknownTemplate : BuildResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal UnknownTemplate(string reason) => Reason = reason;
        }

        /// <summary>Handles every case.</summary>
        public T Match<T>(
            Func<Built, T> built,
            Func<InvalidName, T> invalidName,
            Func<InvalidSpec, T> invalidSpec,
            Func<UnknownTemplate, T> unknownTemplate) => this switch
        {
            Built r => built(r),
            InvalidName r => invalidName(r),
            InvalidSpec r => invalidSpec(r),
            UnknownTemplate r => unknownTemplate(r),
            _ => throw new InvalidOperationException("unreachable: BuildResult is sealed"),
        };

        internal static BuildResult From(EmergenceStatus status, NodeId root, string reason) => status switch
        {
            EmergenceStatus.Ok => new Built(root),
            EmergenceStatus.InvalidName => new InvalidName(reason),
            EmergenceStatus.InvalidSpec => new InvalidSpec(reason),
            EmergenceStatus.UnknownTemplate => new UnknownTemplate(reason),
            _ => throw EmergenceLibrary.Unexpected(status, reason),
        };
    }

    /// <summary>
    /// The result of building a node from a template and connecting it in one step. On any
    /// failure nothing was added to the world.
    /// </summary>
    public abstract class BuildAtResult
    {
        private BuildAtResult() { }

        /// <summary>Built and connected.</summary>
        public sealed class Added : BuildAtResult
        {
            /// <summary>The new node's root.</summary>
            public NodeId Root { get; }
            internal Added(NodeId root) => Root = root;
        }

        /// <summary>The name is empty, reserved, or contains route syntax.</summary>
        public sealed class InvalidName : BuildAtResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal InvalidName(string reason) => Reason = reason;
        }

        /// <summary>The spec is not valid for the template.</summary>
        public sealed class InvalidSpec : BuildAtResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal InvalidSpec(string reason) => Reason = reason;
        }

        /// <summary>No template has that name.</summary>
        public sealed class UnknownTemplate : BuildAtResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal UnknownTemplate(string reason) => Reason = reason;
        }

        /// <summary>Something on that link, or in that parent, already has the name.</summary>
        public sealed class NameConflict : BuildAtResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal NameConflict(string reason) => Reason = reason;
        }

        /// <summary>The parent or link does not exist, or the link belongs to another node.</summary>
        public sealed class CannotPlace : BuildAtResult
        {
            /// <summary>Why.</summary>
            public string Reason { get; }
            internal CannotPlace(string reason) => Reason = reason;
        }

        /// <summary>Handles every case.</summary>
        public T Match<T>(
            Func<Added, T> added,
            Func<InvalidName, T> invalidName,
            Func<InvalidSpec, T> invalidSpec,
            Func<UnknownTemplate, T> unknownTemplate,
            Func<NameConflict, T> nameConflict,
            Func<CannotPlace, T> cannotPlace) => this switch
        {
            Added r => added(r),
            InvalidName r => invalidName(r),
            InvalidSpec r => invalidSpec(r),
            UnknownTemplate r => unknownTemplate(r),
            NameConflict r => nameConflict(r),
            CannotPlace r => cannotPlace(r),
            _ => throw new InvalidOperationException("unreachable: BuildAtResult is sealed"),
        };

        internal static BuildAtResult From(EmergenceStatus status, NodeId root, string reason) => status switch
        {
            EmergenceStatus.Ok => new Added(root),
            EmergenceStatus.InvalidName => new InvalidName(reason),
            EmergenceStatus.InvalidSpec => new InvalidSpec(reason),
            EmergenceStatus.UnknownTemplate => new UnknownTemplate(reason),
            EmergenceStatus.NameConflict => new NameConflict(reason),
            EmergenceStatus.UnknownNode or EmergenceStatus.UnknownLink
                or EmergenceStatus.LinkOwnedElsewhere => new CannotPlace(reason),
            _ => throw EmergenceLibrary.Unexpected(status, reason),
        };
    }

    /// <summary>The result of <see cref="World.Connect"/>. On any failure nothing changed.</summary>
    public abstract class ConnectResult
    {
        private ConnectResult() { }

        /// <summary>Connected.</summary>
        public sealed class Connected : ConnectResult
        {
            internal static readonly Connected Instance = new Connected();
            private Connected() { }
        }

        /// <summary>Another node on that link already has this node's name.</summary>
        public sealed class NameConflict : ConnectResult
        {
            internal static readonly NameConflict Instance = new NameConflict();
            private NameConflict() { }
        }

        /// <summary>The node already has a different parent. Disconnect it first.</summary>
        public sealed class AlreadyHasParent : ConnectResult
        {
            internal static readonly AlreadyHasParent Instance = new AlreadyHasParent();
            private AlreadyHasParent() { }
        }

        /// <summary>The node would end up inside itself.</summary>
        public sealed class WouldCreateCycle : ConnectResult
        {
            internal static readonly WouldCreateCycle Instance = new WouldCreateCycle();
            private WouldCreateCycle() { }
        }

        /// <summary>The root cannot be put inside anything.</summary>
        public sealed class IsRoot : ConnectResult
        {
            internal static readonly IsRoot Instance = new IsRoot();
            private IsRoot() { }
        }

        /// <summary>The link already belongs to a different node.</summary>
        public sealed class LinkOwnedElsewhere : ConnectResult
        {
            internal static readonly LinkOwnedElsewhere Instance = new LinkOwnedElsewhere();
            private LinkOwnedElsewhere() { }
        }

        /// <summary>A node or link ID does not exist (it may have been removed).</summary>
        public sealed class NotFound : ConnectResult
        {
            internal static readonly NotFound Instance = new NotFound();
            private NotFound() { }
        }

        /// <summary>Handles every case.</summary>
        public T Match<T>(
            Func<T> connected,
            Func<T> nameConflict,
            Func<T> alreadyHasParent,
            Func<T> wouldCreateCycle,
            Func<T> isRoot,
            Func<T> linkOwnedElsewhere,
            Func<T> notFound) => this switch
        {
            Connected => connected(),
            NameConflict => nameConflict(),
            AlreadyHasParent => alreadyHasParent(),
            WouldCreateCycle => wouldCreateCycle(),
            IsRoot => isRoot(),
            LinkOwnedElsewhere => linkOwnedElsewhere(),
            NotFound => notFound(),
            _ => throw new InvalidOperationException("unreachable: ConnectResult is sealed"),
        };

        internal static ConnectResult From(EmergenceStatus status) => status switch
        {
            EmergenceStatus.Ok => Connected.Instance,
            EmergenceStatus.NameConflict => NameConflict.Instance,
            EmergenceStatus.AlreadyHasParent => AlreadyHasParent.Instance,
            EmergenceStatus.WouldCreateCycle => WouldCreateCycle.Instance,
            EmergenceStatus.IsRoot => IsRoot.Instance,
            EmergenceStatus.LinkOwnedElsewhere => LinkOwnedElsewhere.Instance,
            EmergenceStatus.UnknownNode or EmergenceStatus.UnknownLink => NotFound.Instance,
            _ => throw EmergenceLibrary.Unexpected(status, ""),
        };
    }

    /// <summary>The result of removing a node or link. On failure nothing changed.</summary>
    public abstract class RemoveResult
    {
        private RemoveResult() { }

        /// <summary>Removed, with everything inside it.</summary>
        public sealed class Removed : RemoveResult
        {
            internal static readonly Removed Instance = new Removed();
            private Removed() { }
        }

        /// <summary>The world's root cannot be removed.</summary>
        public sealed class IsRoot : RemoveResult
        {
            internal static readonly IsRoot Instance = new IsRoot();
            private IsRoot() { }
        }

        /// <summary>It does not exist (it may already have been removed).</summary>
        public sealed class NotFound : RemoveResult
        {
            internal static readonly NotFound Instance = new NotFound();
            private NotFound() { }
        }

        /// <summary>Handles every case.</summary>
        public T Match<T>(Func<T> removed, Func<T> isRoot, Func<T> notFound) => this switch
        {
            Removed => removed(),
            IsRoot => isRoot(),
            NotFound => notFound(),
            _ => throw new InvalidOperationException("unreachable: RemoveResult is sealed"),
        };

        internal static RemoveResult From(EmergenceStatus status) => status switch
        {
            EmergenceStatus.Ok => Removed.Instance,
            EmergenceStatus.IsRoot => IsRoot.Instance,
            EmergenceStatus.UnknownNode or EmergenceStatus.UnknownLink => NotFound.Instance,
            _ => throw EmergenceLibrary.Unexpected(status, ""),
        };
    }
}
