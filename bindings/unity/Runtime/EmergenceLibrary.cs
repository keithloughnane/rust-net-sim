using System;
using System.Runtime.InteropServices;
using Emergence.Native;

namespace Emergence
{
    /// <summary>Information about the loaded native library.</summary>
    public static class EmergenceLibrary
    {
        /// <summary>ABI version these bindings were generated against.</summary>
        public const uint ExpectedAbiVersion = NativeMethods.EMERGENCE_ABI_VERSION;

        /// <summary>ABI version reported by the loaded native library.</summary>
        public static uint AbiVersion => NativeMethods.emergence_abi_version();

        /// <summary>Version string of the loaded native library.</summary>
        public static unsafe string Version =>
            Marshal.PtrToStringUTF8((IntPtr)NativeMethods.emergence_version()) ?? string.Empty;

        private static bool _checked;

        /// <summary>
        /// Throws if the native library's ABI does not match these bindings. Called automatically
        /// before the first object is created.
        /// </summary>
        public static void EnsureCompatible()
        {
            if (_checked) return;
            var actual = AbiVersion;
            if (actual != ExpectedAbiVersion)
            {
                throw new EmergenceException(
                    $"Native library ABI version {actual} does not match bindings version {ExpectedAbiVersion}. " +
                    "Rebuild the native library and bindings together.");
            }
            _checked = true;
        }

        internal static void Check(EmergenceStatus status)
        {
            switch (status)
            {
                case EmergenceStatus.Ok:
                    return;
                case EmergenceStatus.NullPointer:
                    throw new EmergenceException("Native call received a null pointer.");
                case EmergenceStatus.Panic:
                    throw new EmergenceException("Native library hit an internal error.");
                default:
                    throw new EmergenceException($"Native call returned unknown status {(uint)status}.");
            }
        }
    }

    /// <summary>An error reported by the Emergence native library.</summary>
    public sealed class EmergenceException : Exception
    {
        /// <summary>Creates an exception with the given message.</summary>
        public EmergenceException(string message) : base(message) { }
    }
}
