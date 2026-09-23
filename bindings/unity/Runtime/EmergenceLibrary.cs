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
        public static unsafe string Version => FromUtf8(NativeMethods.emergence_version());

        /// <summary>Names of the built-in logic kinds, as a JSON array of strings.</summary>
        public static unsafe string LogicKindsJson => FromUtf8(NativeMethods.emergence_logic_kinds_json());

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

        internal static unsafe void Check(EmergenceStatus status)
        {
            if (status == EmergenceStatus.Ok) return;
            var message = FromUtf8(NativeMethods.emergence_status_message((uint)status));
            throw new EmergenceException(status.ToString(), message);
        }

        internal static unsafe string FromUtf8(byte* utf8) =>
            Marshal.PtrToStringUTF8((IntPtr)utf8) ?? string.Empty;
    }

    /// <summary>An error reported by the Emergence native library.</summary>
    public sealed class EmergenceException : Exception
    {
        /// <summary>The native status code name, such as "UnknownNode", if the error came from a native call.</summary>
        public string Status { get; }

        /// <summary>Creates an exception with the given message.</summary>
        public EmergenceException(string message) : base(message) => Status = string.Empty;

        internal EmergenceException(string status, string message) : base($"{message} ({status})") =>
            Status = status;
    }
}
