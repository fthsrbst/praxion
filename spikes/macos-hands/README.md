# macOS hands spike

Test whether a bounded keyboard hold reaches the running vanilla Minecraft
client and whether physical input ends the hold. This tool does not start a
second player or connect the brain.

From the repository root:

```sh
mkdir -p logs/mac-hands
xcrun swiftc -warnings-as-errors spikes/macos-hands/main.swift -o logs/mac-hands/macos-hands
logs/mac-hands/macos-hands --self-test
logs/mac-hands/macos-hands --preflight
```

Preflight reports the current process's Quartz post and listen permissions and
does not request either. Self-test exercises state transitions and argument
validation without a game or those permissions.

1. Start a disposable local server and use **Join Minecraft**. Leave **Start
   bot** off. Check on the server that the intended test player is the only
   player connected. Use a flat, clear area and confirm that forward is W.
2. Pass the test client's actual JVM PID. Run the command below and return
   to the Minecraft window within ten seconds. The executable check is
   coarse; verify the PID yourself. The spike never activates the window.
3. Verify movement against server coordinates. Once the player settles,
   check that the coordinates stay fixed. Repeat and press Space during
   the hold. Expect `stop_reason=foreign_input`, a shorter hold, one posted
   key-up, a jump, and a stationary player after landing. Exit 3 indicates
   that the hold was interrupted.

```sh
logs/mac-hands/macos-hands --run --pid CLIENT_PID --keycode 13
```

The 1.5 s deadline depends on a responsive run loop. The tool targets only
the supplied PID, checks focus and tap health, and uses a listen-only tap.
It does not retain input contents. `tap_to_key_up_us` measures from callback
receipt to the key-up post call, excluding OS delivery and client handling.
A successful post alone does not prove that Minecraft processed the event.

Use only in a disposable test world. A forced kill or stuck run loop can
prevent release; there is no independent watchdog. Focus loss is covered
by the state test and still needs a live check. Camera, menus, and product
integration are outside this spike.
