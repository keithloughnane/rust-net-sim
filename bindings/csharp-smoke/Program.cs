using System;
using Emergence;

internal static class Program
{
    private static int Main()
    {
        Console.WriteLine($"Emergence {EmergenceLibrary.Version} (ABI {EmergenceLibrary.AbiVersion})");

        using (var world = new World())
        {
            world.Tick();
            world.Tick();
            world.Tick();
            if (world.TickCount != 3)
            {
                Console.Error.WriteLine($"FAIL: expected 3 ticks, got {world.TickCount}");
                return 1;
            }
        }

        var disposed = new World();
        disposed.Dispose();
        try
        {
            disposed.Tick();
            Console.Error.WriteLine("FAIL: using a disposed world did not throw");
            return 1;
        }
        catch (ObjectDisposedException)
        {
        }

        Console.WriteLine("PASS");
        return 0;
    }
}
