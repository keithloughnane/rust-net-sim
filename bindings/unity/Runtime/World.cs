using System;
using Emergence.Native;

namespace Emergence
{
    /// <summary>
    /// A simulation world backed by the native library. Dispose it when finished; the finalizer
    /// is only a safety net.
    /// </summary>
    public sealed unsafe class World : IDisposable
    {
        private EmergenceWorld* _handle;

        /// <summary>Creates an empty world at tick zero.</summary>
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
    }
}
