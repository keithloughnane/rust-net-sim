#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;
using Emergence;
using Emergence.Host;

// A headless host: C# nodes exchanging packets over Emergence, the way a game engine runs them,
// with no engine.
internal static class Program
{
    private static int _failures;

    private static void Expect(bool condition, string what)
    {
        if (condition) return;
        Console.Error.WriteLine($"FAIL: {what}");
        _failures++;
    }

    /// <summary>Logic that remembers what it received and sends when told to.</summary>
    private sealed class Recorder : NodeLogic
    {
        public readonly List<object?> Received = new List<object?>();

        public override void OnReceived(Packet packet, PacketRoute selfRoute, LogicNode thisNode,
            IReadOnlyList<NodeModel> internalNodes, IReadOnlyList<LinkModel> internalLinks,
            IReadOnlyList<string> externalLinkIds) => Received.Add(packet.Data);

        public void Say(PacketRoute from, PacketRoute to, object data) => Send(from, to, data);
    }

    private sealed class TestNode : LogicNode
    {
        public readonly Recorder Logic = new Recorder();

        public TestNode(string id) : base(id)
        {
        }

        protected override NodeLogic CreateLogic() => Logic;
    }

    private static void Steps(int count)
    {
        for (var i = 0; i < count; i++) HostNetwork.Step();
    }

    private static int Main()
    {
        HostLog.OnWarning = (tag, message) => Console.Error.WriteLine($"[{tag}] warning: {message}");
        var errors = new List<string>();
        HostLog.OnError = (tag, message) => errors.Add($"[{tag}] {message}");

        var root = new NodeModel(Addresses.Root);
        var bus = new LinkModel("bus");
        var a = new TestNode("a");
        var b = new TestNode("b");
        HostNetwork.Connect(root, a, bus);
        HostNetwork.Connect(root, b, bus);
        Expect(a.Parent == root && root.InternalNodes.Contains(b), "nested in the root");
        Expect(a.Links.Contains(bus) && bus.Members.Count == 2, "both on the bus");

        // A packet over a link arrives on a later tick, and not back at its sender.
        var watched = 0;
        using (b.ObservePacketsIntoNode().Subscribe(new Observer(() => watched++)))
        {
            a.Logic.Say(new PacketRoute("a", "bus"), new PacketRoute("b", "bus"), "hello");
            Expect(b.Logic.Received.Count == 0, "nothing arrives before a tick");
            Steps(2);
        }
        Expect(b.Logic.Received.SequenceEqual(new object[] { "hello" }), "b got hello");
        Expect(a.Logic.Received.Count == 0, "a did not hear itself");
        Expect(watched == 1, "the accepted packets can be watched");

        // Addressed to someone else: b's accept rules turn it away.
        a.Logic.Say(new PacketRoute("a", "bus"), new PacketRoute("c", "bus"), "not for b");
        Steps(2);
        Expect(b.Logic.Received.Count == 1, "b ignores packets for others");

        // A direct push arrives at once, and is counted.
        b.PushDirect(new Packet(new PacketRoute("x", "y"), new PacketRoute("b", "y"), "now"));
        Expect(b.Logic.Received.LastOrDefault() as string == "now", "a direct push arrives at once");
        Expect(HostNetwork.DirectPushCounts().Values.Sum() == 1, "direct pushes are counted");

        // What a node sends just before leaving still goes out; then it hears nothing.
        b.Logic.Say(new PacketRoute("b", "bus"), new PacketRoute("a", "bus"), "bye");
        HostNetwork.Disconnect(root, b, bus);
        Expect(b.Parent == null && b.Links.Count == 0 && !bus.Members.Contains(b), "b has left");
        a.Logic.Say(new PacketRoute("a", "bus"), new PacketRoute("*", "*"), "anyone?");
        Steps(3);
        Expect(a.Logic.Received.SequenceEqual(new object[] { "bye" }), "b's last words got out");
        Expect(!b.Logic.Received.Contains("anyone?"), "b hears nothing after leaving");

        // A computer built from an Emergence template, adopted as a C# node.
        var built = HostNetwork.BuildTemplateAt("computer", "pc", root, bus);
        var pcId = built.Match(
            added: r => r.Root,
            invalidName: r => default,
            invalidSpec: r => default,
            unknownTemplate: r => default,
            nameConflict: r => default,
            cannotPlace: r => default);
        Expect(pcId.IsValid, $"the template was built: {built.GetType().Name}");
        var pc = new TestNode("pc");
        HostNetwork.Adopt(pcId, pc);
        Expect(pc.Parent == root && root.InternalNodes.Contains(pc), "the adopted node knows its parent");
        Expect(pc.Links.Contains(bus) && bus.Members.Contains(pc), "the adopted node is on the bus");
        Expect(pc.InternalLinks.Any(l => l.ID == Addresses.Ipc), "the adopted node owns its ipc bus");
        a.Logic.Say(new PacketRoute("a", "bus"), new PacketRoute("pc", "bus"), "hi pc");
        Steps(2);
        Expect(pc.Logic.Received.Contains("hi pc"), "the adopted node hears its links");

        Steps(1);
        var drift = HostNetwork.VerifyStructure();
        Expect(drift.Count == 0, $"C# and Emergence agree: {string.Join("; ", drift)}");
        Expect(errors.Count == 0, $"no errors logged: {string.Join("; ", errors)}");

        if (_failures > 0)
        {
            Console.Error.WriteLine($"{_failures} host check(s) failed");
            return 1;
        }
        Console.WriteLine("PASS (host)");
        return 0;
    }

    private sealed class Observer : IObserver<Packet>
    {
        private readonly Action _next;

        public Observer(Action next) => _next = next;

        public void OnNext(Packet value) => _next();

        public void OnError(Exception error) => throw error;

        public void OnCompleted()
        {
        }
    }
}
