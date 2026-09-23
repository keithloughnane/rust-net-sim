# Emergence Engine

A hierarchical packet-routing network simulation, reimplemented in Rust from the SmitherNet
design in *Smithereen Cold Boot Attack*. It ships as a native library with a C ABI, so it can be
used from Unity (C#), Unreal (C++) or any other engine.

## Concepts

- **`ControlNode`**: a device, service, app or container. Nodes form a tree: each has at most one
  parent. The tree's top is the root node (`.`), shown as "world" in the sandbox.
- **`Link`**: a shared broadcast medium (a Wi-Fi zone, a cable, a computer's `ipc` bus). Any
  number of nodes can subscribe to it. A link can be *internal* to one node, which is where its
  children talk to each other. Links owned by the root are world links.
- **`Network`**: holds every node and link, addressed by `NodeId`/`LinkId` handles. It is built
  with `create_node`, `create_link`, `connect`, `subscribe`, `unsubscribe`, `disconnect` and
  `add_internal_link`. Invalid operations (a second parent, a cycle, a link already owned
  elsewhere) are rejected with a typed error and change nothing.
- **`ControllerLogic`**: behaviour attached to a node. It has `on_tick` and `on_received`
  hooks, and sends through a `Context` (`send`, `reply`, `forward`). Built-in kinds:
  `responder` (answers `ping` with `pong`), `gateway` (connects a computer's internal bus to
  the outside), `beacon` (pings every link it is on, periodically) and `scanner` (an app that
  pings out through its computer's gateway).
- **`Packet`**: `from` and `to` routes plus an `Event` (a `kind` string and opaque bytes), with a
  TTL (16) and a trace of the nodes it passed through. Routes are hop stacks, written
  `node@link/node@link`. `*` addresses everyone and `^` the parent.
- **`World`**: a network, its logic and the clock. The host calls `tick()`: each tick runs
  every `on_tick`, then delivers everything queued before it. Replies wait for the next tick, so
  packets move one hop per tick, the same inputs always give the same result, and a tick
  always finishes. `drain_trace()` reports what happened (sent, delivered, dropped, notes).

### Safety: monitor and fuse

Runaway networks (loops, replay storms, amplification, floods) have crashed Unity and whole
machines before, so the engine has two layers of defence:

- **Monitor** (`MonitorConfig`): observes and raises `Alert`s (warning or error) into the trace.
  Checks: network, link and node rate; TTL expiry (a loop has happened); relay cycle (a loop
  *will* happen, found from the topology before any traffic); replay (the same packet over and
  over); amplification (one packet in, many out); logic panicked. Alerts are de-duplicated so
  they cannot flood the trace themselves.
- **Fuse** (`Limits`): hard per-tick limits on packets sent, packets delivered, queue size and
  payload size. A tick that hits one stops delivering, keeps the rest queued, and returns
  `TickOutcome::FuseTripped` (`EMERGENCE_STATUS_FUSE_TRIPPED` over the C ABI) with a
  `FuseReport`: the limit, the busiest senders and links, and the latest alert from each check,
  earliest (usually the root cause) first. The library only reports; the host decides what to do.
  The sandbox pauses the world and shows the report. Even a host that ignores the fuse and keeps
  ticking stays within the limits.

Also: logic that panics is removed and reported instead of taking the world down, packets share
their payload instead of copying it to every listener, trace and replay memory are bounded, and
names are validated. Names can't be empty or reserved, can't contain `@` or `/`, and can't clash
with another node on the same link. Every computer can still have its own `registry`.

### Routing

A node accepts a packet on a link if it is addressed to it or to everyone (`*`), if a child
sent it to `^` on a link the node owns, or, as a **gateway**, if it arrived on a link the node
owns and its next hop is on a different link. A link's owner hears its own buses. When a
gateway forwards a packet outwards, it adds itself to the front of the `from` route, so replies
come back through it and are popped back inside automatically.

Deliberate differences from the SmitherNet design docs:

- **The gateway rule checks only the next hop, not every hop.** Checking every hop made a
  node intercept replies meant for its own children's nested nodes.
- **Packets go out on the link the sender names in its `from` route**, not on every link it is
  attached to. There are no per-link accept filters yet (`strict`/`allowEverything`).
- **There is no wall-clock age limit.** Every reply waits for the next tick, so a tight loop
  can't cascade inside one call; the TTL catches loops.

## Layout

| Path | Purpose |
|---|---|
| `crates/emergence-engine` | The engine itself. Pure Rust, no UI, no unsafe code. |
| `crates/emergence-ffi` | C ABI over the engine. Builds `libemergence` (`.dylib`/`.dll`/`.so` and a static lib). |
| `crates/emergence-sandbox` | Desktop test UI (egui). Loads the compiled `libemergence` at runtime, exactly like a game engine would. |
| `bindings/c/emergence.h` | Generated C/C++ header (Unreal, custom engines). |
| `bindings/unity/` | Unity package: generated P/Invoke bindings plus a safe C# wrapper. |
| `bindings/csharp-smoke/` | Compiles the Unity bindings with .NET and runs them against the real library. |
| `xtask/` | Build tasks (`cargo xtask help`). |

## Common commands

```sh
cargo build                 # engine + native library only (the workspace default)
cargo build --workspace     # everything, including the sandbox
cargo test --workspace      # all tests (sandbox tests need the native library built first)
cargo doc --open            # API docs

cargo sandbox               # build the native library and run the test UI against it
cargo xtask bindings        # regenerate the C header and C# bindings after changing the ABI
cargo xtask test-csharp     # run the C# bindings against the native library (needs dotnet)
cargo xtask dist            # package a release build into dist/

cargo fmt --all             # format
cargo check-all             # clippy on everything, warnings as errors
```

## Sandbox

`cargo sandbox` builds each test network through the C ABI, runs it, and draws it one level at a
time:

- **Canvas**: the children of the current node, with each link drawn as a coloured bus and a
  spoke to each subscriber. Links owned outside the current level have a dashed outline. Nodes
  with something inside show a stacked edge and a count; double-click to open them.
- **Hierarchy** (left): the whole tree. Click to select and jump to that level.
- **Inspector** (right): details of the selection, with clickable parents, children and links.
- **Traffic**: packets appear as dots travelling sender → link → receiver (cyan ping, orange
  pong). Links glow while carrying a packet and nodes glow when one arrives. Traffic inside a
  node you are not looking at makes that node glow. Node boxes show their logic and `↑sent
  ↓received` counts.
- **Traffic log** (bottom): one line per packet, with who accepted it and why (gateway, child to
  parent), drops, and notes from logic. It can be filtered to the selected node.
- **Alerts** (bottom tab, plus `!` badges on the canvas): everything the monitor raised.
- **Fuse**: when a tick hits a hard limit, the world pauses and a banner explains why. Use
  *Limits…* in the toolbar to try different limits live.
- **Tests** (bottom tab): the stress-test checklist. Run one test or all of them on a background
  thread and see every check pass or fail. *Watch* loads a test's network into the viewer,
  paused, so you can step through exactly what it does.
- **Controls**: play/pause (Space), step one tick, and speed. The inspector can change a node's
  logic and send events from it: one click pings everyone on a link, or fill in a route such
  as `pc-manager@office-wifi/fileman@ipc`.
- Scroll to zoom, drag the background to pan, drag an item to move and pin it, Esc to go up.

Test networks live in `crates/emergence-sandbox/src/scenarios.rs`. Stress tests live in
`crates/emergence-sandbox/src/stress.rs`, grouped as healthy traffic (no false alarms), loops,
replay and amplification, floods and limits, player edits (bad names, clashes, unplugging
mid-flight, 2,000-deep nesting, 20,000 nodes, odd addresses), and robustness (crashing logic,
determinism). `cargo test --workspace` runs the whole catalogue against the real library. To add
a case, add an entry to `ALL` with a `setup` (build the network) and a `verify` (run and check).

Deliberately broken logic kinds exist for these tests and are marked as faulty in the UI:
`echo`, `replayer`, `amplifier`, `flooder`, `crasher`.

### Driving the sandbox from a terminal

While `cargo sandbox` is open, `cargo ctl` drives the same window from another terminal (or a
script, or an AI assistant). Everything happens visibly in the window, and the results print
in the terminal:

```sh
cargo ctl status                        # what is loaded, tick, load, alerts, fuse, tests
cargo ctl load office                   # switch scenario
cargo ctl step 30                       # run 30 ticks now and summarise them
cargo ctl open pc-manager               # move the canvas inside a node
cargo ctl send laptop office-wifi "pc-manager@office-wifi/fileman@ipc" ping hello
cargo ctl log 10                        # the last traffic lines
cargo ctl watch four bridges            # load a stress test's network, paused
cargo ctl step 20                       # stops early, and explains, if the fuse trips
cargo ctl run                           # run the whole checklist and print the results
cargo ctl screenshot window.png         # save a picture of the window
cargo ctl help                          # every command
```

Add `--json` to any command for structured output. The exit code is non-zero when a command
fails (including `run` with failing tests), so it works in scripts and CI. The control port
listens on 127.0.0.1:47474 only; set `EMERGENCE_CONTROL_PORT` to use another port, or 0 to turn
it off. The protocol is one JSON object per line (see
`crates/emergence-sandbox/src/control.rs`).

Environment variables for scripted runs:

| Variable | Effect |
|---|---|
| `EMERGENCE_LIB` | Load the library from this path instead of next to the executable. |
| `EMERGENCE_SCENARIO` | Start with this scenario, e.g. `Mesh`. |
| `EMERGENCE_OPEN` | Start inside the first node with this name, e.g. `pc-manager`. |
| `EMERGENCE_SPEED` | Start at this many ticks per second. |
| `EMERGENCE_WATCH` | Start by watching this stress test, e.g. `Firehose`. |
| `EMERGENCE_PLAY` | Start playing even when watching a test. |
| `EMERGENCE_TAB` | Start on the `traffic`, `alerts` or `tests` tab. |
| `EMERGENCE_RUN_TESTS` | Run the whole stress-test checklist at startup. |
| `EMERGENCE_SCREENSHOT_FRAMES` | Frames to wait before the screenshot (default 150). |
| `EMERGENCE_SCREENSHOT` | Save a PNG of the window once the layout settles, then quit. |

## Using the library

### Unity

1. `cargo xtask dist`
2. In Unity: *Window → Package Manager → + → Install package from disk…* and pick
   `dist/unity/com.keithloughnane.emergence/package.json`.
3. Use it:

   ```csharp
   using Emergence;

   using var world = new World();
   var wifi = world.CreateLink("wifi-1");
   var pc = world.CreateNode("pc-1", "computer");
   var laptop = world.CreateNode("laptop", "computer");
   world.Connect(world.Root, pc, wifi);
   world.Connect(world.Root, laptop, wifi);
   world.SetLogic(pc, "responder");

   world.Send(laptop, "wifi-1", "pc-1@wifi-1", "ping");
   if (world.Tick() == TickOutcome.FuseTripped)   // pc-1 gets the ping and answers
       Debug.LogError(world.FuseReportJson());    // the last line of defence: pause and look
   world.Tick();                                  // laptop gets the pong
   Debug.Log(world.DrainTraceJson());
   ```

`dist` only contains the native library for the machine that built it. Other platforms need their
own build (or cross-compilation) added to `Runtime/Plugins/`. On iOS the bindings automatically
switch to the statically linked `__Internal` library.

### Unreal / C++

Include `dist/c/include/emergence.h` and link `libemergence` from `dist/c/lib/<platform>/` via a
ThirdParty module. The header is plain C with `extern "C"` guards, so it works from C++ unchanged.

## Changing the C ABI

1. Edit `crates/emergence-ffi/src/lib.rs`. Keep to its conventions: `emergence_` prefix, opaque
   handles, status codes, no panics across the boundary.
2. If you changed or removed anything existing, bump `EMERGENCE_ABI_VERSION`.
3. `cargo xtask bindings` and commit the regenerated files.
4. Update the safe wrappers: `bindings/unity/Runtime/*.cs` and
   `crates/emergence-sandbox/src/native.rs`. If the snapshot shape changed, bump its `FORMAT`
   and update `crates/emergence-sandbox/src/snapshot.rs`.
5. `cargo build --workspace && cargo test --workspace && cargo xtask test-csharp && cargo check-all`.
