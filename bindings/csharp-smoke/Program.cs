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

            // Traffic: the laptop pings the app inside the PC; the PC routes it in, the app
            // answers, and the PC routes the pong back out.
            var laptop = world.CreateNode("laptop", "computer");
            world.Connect(root, laptop, wifi);
            world.SetLogic(pc, "gateway");
            world.SetLogic(app, "responder");
            world.DrainTraceJson();
            world.Send(laptop, "wifi-1", "pc-1@wifi-1/fileman@ipc", "ping", new byte[] { 104, 105 });
            for (var i = 0; i < 4; i++) world.Tick();
            var trace = world.DrainTraceJson();
            Expect(trace.Contains("\"kind\":\"pong\""), "the app answered the ping");
            Expect(world.SnapshotJson().Contains("\"logic\":\"gateway\""), "snapshot shows logic");
            Expect(EmergenceLibrary.LogicKindsJson.Contains("responder"), "logic kinds listed");

            // A broadcast storm: bridges joining two links in a loop. The fuse must trip, and the
            // report must say why, instead of the host running out of memory.
            var linkA = world.CreateLink("storm-a");
            var linkB = world.CreateLink("storm-b");
            var shouter = world.CreateNode("shouter", "device");
            world.Connect(root, shouter, linkA);
            for (var i = 0; i < 4; i++)
            {
                var bridge = world.CreateNode($"bridge-{i}", "hub");
                world.Connect(root, bridge, linkA);
                world.Subscribe(bridge, linkB);
                world.SetLogic(bridge, "bridge");
            }
            world.SetTracePackets(false);
            world.Send(shouter, "storm-a", "*@storm-a", "hello");
            var tripped = false;
            for (var i = 0; i < 30 && !tripped; i++) tripped = world.Tick() == TickOutcome.FuseTripped;
            Expect(tripped, "the storm tripped the fuse");
            Expect(world.FuseReportJson().Contains("relay_cycle"), "the fuse report names the relay loop");
            Expect(world.HealthJson().Contains("\"fuse_trips\":1"), "health counts the trip");

            try
            {
                world.CreateNode("bad@name", "x");
                Expect(false, "invalid names throw");
            }
            catch (EmergenceException e)
            {
                Expect(e.Status == "InvalidName", $"invalid name status (got {e.Status})");
            }

            try
            {
                world.SetLogic(pc, "teleporter");
                Expect(false, "unknown logic throws");
            }
            catch (EmergenceException e)
            {
                Expect(e.Status == "UnknownLogic", $"unknown logic status (got {e.Status})");
            }

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
