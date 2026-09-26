using Emergence.Host;
using Unity.Profiling;
using UnityEngine;

namespace Emergence.Unity
{
    /// <summary>
    /// Runs <see cref="HostNetwork"/> in Unity: one <see cref="HostNetwork.Step"/> every
    /// <see cref="HostNetwork.TickInterval"/> on the main thread, and the host layer's log in the
    /// Unity console. Created when the game starts and kept across scenes. A game that has its own
    /// logger sets <see cref="HostLog"/>'s hooks in its own start-up (BeforeSceneLoad or later).
    /// </summary>
    public sealed class EmergenceDriver : MonoBehaviour
    {
        private static readonly ProfilerMarker StepMarker = new ProfilerMarker("Emergence.HostNetwork.Step");

        private float _elapsed;

        [RuntimeInitializeOnLoadMethod(RuntimeInitializeLoadType.SubsystemRegistration)]
        private static void LogToConsole()
        {
            HostLog.OnDebug = null;
            HostLog.OnWarning = (tag, message) => Debug.LogWarning($"[{tag}] {message}");
            HostLog.OnError = (tag, message) => Debug.LogError($"[{tag}] {message}");
        }

        [RuntimeInitializeOnLoadMethod(RuntimeInitializeLoadType.BeforeSceneLoad)]
        private static void Create()
        {
            var host = new GameObject("Emergence") { hideFlags = HideFlags.HideInHierarchy };
            DontDestroyOnLoad(host);
            host.AddComponent<EmergenceDriver>();
            // In the editor, check every 5 seconds that C#'s view of the structure matches.
            HostNetwork.VerifyStructureEveryTicks = Application.isEditor ? 100 : 0;
        }

        private void Update()
        {
            _elapsed += Time.unscaledDeltaTime;
            var interval = (float)HostNetwork.TickInterval.TotalSeconds;
            // Catch up after a slow frame, but never spiral: at most a few steps a frame.
            for (var steps = 0; _elapsed >= interval && steps < 4; steps++)
            {
                _elapsed -= interval;
                using (StepMarker.Auto())
                {
                    HostNetwork.Step();
                }
            }
            if (_elapsed > interval) _elapsed = 0;
        }
    }
}
