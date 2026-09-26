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

    private static int CountNodes(World world)
    {
        var json = world.SnapshotJson();
        var count = 0;
        for (var i = json.IndexOf("\"kind\":", StringComparison.Ordinal); i >= 0;
             i = json.IndexOf("\"kind\":", i + 1, StringComparison.Ordinal))
        {
            count++;
        }
        return count;
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

            // Templates: a computer from the catalogue, attached to the Wi-Fi, reachable inside.
            var built = world.BuildComputer("pc-9", new[] { "fileman", "net-scan" }, new[] { "wifi" });
            var pc9 = built.Match(
                built: b => b.Root,
                invalidName: e => throw new Exception(e.Reason),
                invalidSpec: e => throw new Exception(e.Reason),
                unknownTemplate: e => throw new Exception(e.Reason));
            Expect(world.Connect(root, pc9, wifi) is ConnectResult.Connected, "template computer connected");
            var snapshot = world.SnapshotJson();
            Expect(snapshot.Contains("\"name\":\"pc-9\"") && snapshot.Contains("\"name\":\"drive-bay\""), "template computer built");
            Expect(EmergenceLibrary.TemplatesCatalogJson.Contains("net-scan"), "catalogue lists apps");
            var doom = world.BuildComputer("pc-10", new[] { "doom" });
            Expect(doom is BuildResult.InvalidSpec s1 && s1.Reason.Contains("doom"),
                "an unknown app is an InvalidSpec result that explains itself");

            // One step: a name clash means nothing is added. Two steps: the node stays built.
            var nodesBefore = CountNodes(world);
            var clash = world.BuildComputerAt("pc-9", root, wifi);
            Expect(clash is BuildAtResult.NameConflict, $"one-step clash is a NameConflict (got {clash.GetType().Name})");
            Expect(CountNodes(world) == nodesBefore, "one-step failure leaves nothing behind");
            var twin = (BuildResult.Built)world.BuildComputer("pc-9");
            Expect(world.Connect(root, twin.Root, wifi) is ConnectResult.NameConflict, "two-step connect clash");
            Expect(CountNodes(world) > nodesBefore, "two-step: the built node stays");
            Expect(world.RemoveNode(twin.Root) is RemoveResult.Removed, "and can be removed");
            Expect(CountNodes(world) == nodesBefore, "removing it removes everything inside");
            Expect(world.RemoveNode(twin.Root) is RemoveResult.NotFound, "removing twice is NotFound");
            Expect(world.RemoveNode(root) is RemoveResult.IsRoot, "the root cannot be removed");
            var added = world.BuildComputerAt("pc-11", root, wifi, new[] { "mail" });
            Expect(added.Match(
                added: _ => true,
                invalidName: _ => false,
                invalidSpec: _ => false,
                unknownTemplate: _ => false,
                nameConflict: _ => false,
                cannotPlace: _ => false), "one-step add succeeds");

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

            Expect(world.Connect(app, pc) is ConnectResult.AlreadyHasParent, "a second parent is refused");
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
