using System;
using Emergence;

internal static class Program
{
    private static int _failures;

    private static void Expect(bool condition, string what)
    {
        if (condition) return;
        Console.Error.WriteLine($"FAIL: {what}");
        _failures++;
    }

    private static int Main()
    {
        Console.WriteLine($"Emergence {EmergenceLibrary.Version} (ABI {EmergenceLibrary.AbiVersion})");

        using (var world = new World())
        {
            world.Tick();
            world.Tick();
            Expect(world.TickCount == 2, "tick count");

            var root = world.Root;
            var wifi = world.CreateLink("wifi-1");
            var pc = world.CreateNode("pc-1", "computer");
            var ipc = world.CreateLink("ipc");
            var app = world.CreateNode("fileman", "app");
            world.Connect(root, pc, wifi);
            world.Connect(pc, app, ipc);
            Expect(root.IsValid && pc != app, "IDs are valid and distinct");

            var json = world.SnapshotJson();
            Expect(json.Contains("\"name\":\"fileman\""), "snapshot contains the app");
            Expect(json.Contains("\"name\":\"wifi-1\""), "snapshot contains the link");

            try
            {
                world.Connect(app, pc);
                Expect(false, "giving a node a second parent throws");
            }
            catch (EmergenceException e)
            {
                Expect(e.Status == "AlreadyHasParent", $"second-parent error status (got {e.Status})");
            }
        }

        var disposed = new World();
        disposed.Dispose();
        try
        {
            disposed.Tick();
            Expect(false, "using a disposed world throws");
        }
        catch (ObjectDisposedException)
        {
        }

        Console.WriteLine(_failures == 0 ? "PASS" : $"{_failures} FAILURE(S)");
        return _failures == 0 ? 0 : 1;
    }
}
