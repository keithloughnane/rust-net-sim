#nullable enable
using System;

namespace Emergence.Host
{
    /// <summary>
    /// Where the host layer's log messages go, as (tag, message). Connect these to the engine's or
    /// game's logger. By default, debug messages are dropped and warnings and errors go to the
    /// standard error stream; the Emergence.Unity assembly sends them to the Unity console.
    /// </summary>
    public static class HostLog
    {
        /// <summary>Detail, such as nodes joining links. Null to drop.</summary>
        public static Action<string, string>? OnDebug { get; set; }

        /// <summary>Something unexpected that the network recovered from. Null to drop.</summary>
        public static Action<string, string>? OnWarning { get; set; } =
            (tag, message) => Console.Error.WriteLine($"[{tag}] warning: {message}");

        /// <summary>A bug, or traffic that was lost. Null to drop.</summary>
        public static Action<string, string>? OnError { get; set; } =
            (tag, message) => Console.Error.WriteLine($"[{tag}] error: {message}");

        internal static void Debug(string tag, string message) => OnDebug?.Invoke(tag, message);

        internal static void Warning(string tag, string message) => OnWarning?.Invoke(tag, message);

        internal static void Error(string tag, string message) => OnError?.Invoke(tag, message);
    }
}
