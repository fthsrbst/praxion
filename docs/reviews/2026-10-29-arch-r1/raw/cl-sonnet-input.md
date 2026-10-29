<!-- Reviewer: Claude Sonnet 5.5 (Claude Code workflow subagent). Lens: sonnet-input. Raw, unverified output. -->

### Findings

**WIN-1 | major | 02 §2.4 "Minor picks", architecture §7 "Take-over [D]"**
- **Claim:** The take-over rule "physical = not injected, so everything injected is Praxion" is inverted. The safe rule is "Praxion's own events carry a tag, and anything else is the user."
- **Evidence:**
  - Microsoft defines `LLKHF_INJECTED` and `LLMHF_INJECTED` as "injected (from any process)".
  - Input from remote-control tools usually reaches the machine as injected input. For example, Chromium's remoting host calls `SendInput` per event.
  - I could not confirm how Parsec, Sunshine, AnyDesk or RDP flag their input (unverified).
  - Session 2 says RDP blocks raw relative input. So the user's remote route is Parsec/Sunshine-class tools, which have not been analysed.
  - `KBDLLHOOKSTRUCT` and `MSLLHOOKSTRUCT` both expose `dwExtraInfo`. `KEYBDINPUT` and `MOUSEINPUT` let the sender set it. The docs never use this field, so there is no ownership tag.
  - `kanata`'s Windows hook (it filters injected events with the system flag and does not use `dwExtraInfo`) shows the pattern the docs follow, not the safer one.
- **Recommendation:**
  - Put a per-run random nonce in `dwExtraInfo` of every injected event.
  - Treat "no nonce" as the user, whether the event is injected or not.
  - Add to E12: take-over from a Parsec/Sunshine/AnyDesk session, and from a second injector such as AutoHotkey.
- **Confidence:** high on the design flaw, medium on remote tools actually being flagged as injected.

**WIN-2 | major | 02 §2.4 (hook on its own thread), architecture §3 "Take-over"**
- **Claim:** A low-level hook can die without telling you, and Microsoft itself recommends raw input instead.
- **Evidence:**
  - LowLevelKeyboardProc docs: "on Windows 7 and later, the hook is silently removed... There is no way for the application to know."
  - Same page: "In most cases where the application needs to use low level hooks, it should monitor raw input instead."
  - The hook runs in the installing thread's context. The injecting thread waits for it ("context switches back to the application that generated the event"), so every `SendInput` is coupled to the hook thread.
  - Hooks overview: hooks "tend to slow down the system".
- **Recommendation:**
  - Take-over depends on a sensor that can fail silently, so add a liveness check. Periodically inject a tagged probe event (an unbound key such as F24, or a zero-delta mouse move) and require the hook to see it within a deadline. If it does not, reinstall the hook, stop injecting and tell the user.
  - Consider raw input (`RIDEV_INPUTSINK`, message-only window) as the primary physical-input sensor. Two things must be tested before relying on it:
    - Whether `hDevice` is 0 for injected events. The 02 doc marks this "inferred". I could not confirm it, and a search snippet claims raw input does not distinguish injected input.
    - Whether `RAWKEYBOARD.ExtraInformation` and `RAWMOUSE.ulExtraInformation` carry the `SendInput` `dwExtraInfo`. If they do, raw input plus the WIN-1 tag is enough, with no hook.
- **Confidence:** high on the silent removal, low on the raw-input ownership claims.

**WIN-3 | major | 02 §2.3 H3 ("checks the integrity level at start")**
- **Claim:** A start-time integrity check is not enough, and failures are silent.
- **Evidence:**
  - SendInput docs: "neither GetLastError nor the return value will indicate the failure was caused by UIPI blocking."
  - The UIAccess page says a process without UIAccess "cannot access elevated... process UI". Hooks and raw input are blind in that case too (AutoHotkey reports in the search result, not directly read).
  - An elevated app (Task Manager, an installer, a UAC prompt) can take the foreground mid-session.
  - Evidence level 0 in session 4 is "sent (input injected)". Under UIPI that level is false without any error.
  - UIAccess is not a way out. It needs an Authenticode signature and an install under Program Files.
- **Recommendation:**
  - Before hand-over, and then continuously, check the integrity level of the foreground window's process and confirm it is the game window.
  - Treat "no pose or echo in the proxy within N ticks of input" as `input_blocked`: release keys, stop, tell the user.
  - Do not use UIAccess.
- **Confidence:** high.

**WIN-4 | major | 06 (bridge-edge sneak), 04 (`unsafe_until`), 02 §2.3**
- **Claim:** Sneak taps on Shift may trigger the Windows Sticky Keys shortcut. Its dialog steals focus, and that triggers H3 and the 500 ms pause menu.
- **Evidence:**
  - `STICKYKEYS` docs: `SKF_HOTKEYACTIVE` lets the user toggle the feature "by pressing the SHIFT key five times".
  - A Unity thread reports the prompt hanging a fullscreen game.
  - Nothing I found says whether injected Shift presses count. I could not confirm that they do.
- **Recommendation:**
  - Add an experiment: 5 or more injected Shift taps in a row, with the hotkey active.
  - Add an environment check that reads, and never writes, `SPI_GETSTICKYKEYS`, `SPI_GETFILTERKEYS` and `SPI_GETTOGGLEKEYS`, and refuses to start with a message. This fits the "no system settings changes" rule.
  - If the trigger is confirmed, bridging needs a different sneak scheme: hold Shift, or put a non-Shift key between taps (also unverified).
- **Confidence:** low on the trigger, high that nothing has covered it.

**WIN-5 | major | 02 §2.4 (lease and watchdog), architecture §3**
- **Claim:** The watchdog design is under-specified for the failure that matters.
- **Evidence:**
  - SendInput docs: it "does not reset the keyboard's current state."
  - Forum reports say a force-killed injector leaves keys logically down (low-quality sources, search results only).
  - GLFW `_glfwInputWindowFocus` releases keys and buttons only on focus loss, as synthetic events inside GLFW. It does not touch the Windows-level state.
  - GLFW `_glfwInputKey` drops a release for a key that is not pressed, so extra key-ups are harmless to the game.
- **Recommendation:**
  - Make the watchdog wait on the body's process handle (`WaitForSingleObject`). That reacts at once to a crash. Leases then only cover a hung body.
  - Keep the held-key table in shared memory (seqlock) or a log the watchdog can replay. It must hold keys and mouse buttons. The common stuck case is a held left button while mining, not a key. E12 test 4 covers keys only.
  - Have the watchdog and body supervise each other, so a dead watchdog is noticed.
  - Release by the same rule as take-over: only what Praxion holds and the user does not physically hold.
  - On every body start, run an idempotent "release everything we may have held".
  - Session 4's exception (hold keys in `unsafe_until` moments) means a crash during a dangerous hold releases the key at the worst moment. State this or test it.
- **Confidence:** high.

**WIN-6 | major | 02 §2.7 E7, 06 §Execution, 02 §2.3 H1**
- **Claim:** "One-tick hold, at least 95 of 100 exactly one tick" cannot be met with an open-loop 50 ms hold. The criterion and the mechanism are mismatched.
- **Evidence (26.2 decompiled mirror, not 26.3):**
  - `Minecraft.runTick` takes the tick count from a real-time delta tracker and runs `for (i < Math.min(10, ticksToDo)) tick()`.
  - Ticks therefore start at render-frame boundaries. At 60 fps that is 16.7 ms steps, with bursts after stalls.
  - `handleKeybinds` runs inside `tick()`. Movement keys are sampled as held state at tick time.
  - A fixed 50 ms hold can span 0, 1 or 2 samples. In a catch-up burst, several ticks see the same state.
- **Recommendation:**
  - Make the proxy's per-tick pose packets the clock: press, wait for the first pose packet that shows the effect, release.
  - Define E7 as "closed-loop hold yields exactly one tick", and run it at 30, 60 and 144 fps, with chunk-load stalls.
  - Check the same thing for E8's click timing. Attack and use are counted via `consumeClick` per tick.
- **Confidence:** medium.

**WIN-7 | major | 02 §2.3 H2, §2.7 E5**
- **Claim:** Gain estimation from `options.txt` plus closed-loop correction can fail if acceleration is applied to injected moves, because the error depends on speed, not just a gain.
- **Evidence:**
  - MOUSEINPUT docs: relative `SendInput` motion is "subject to the effects of the mouse speed and the two-mouse threshold values", multiplied by up to four.
  - Raw-input docs say raw input is not subject to the Control Panel mouse speed. Whether this holds for synthesized raw records is unverified, and session 2 says the same.
  - MC-230466 (search snippet only): a PyDirectInput user got a wrong camera with raw input on in 1.17. Nothing in it explains the mechanism.
  - The game's `rawMouseInput` option and Windows pointer settings are user-controlled.
- **Recommendation:**
  - E5 should be a matrix: raw on/off × pointer speed (slider default and others) × Enhance Pointer Precision on/off × per-move sizes of 1, 5, 50 and 500 counts. Check linearity, not one gain figure.
  - Refuse to start if raw input is off and EPP is on, unless the matrix shows linearity.
  - Also test a physically resting user mouse alongside injected moves.
- **Confidence:** medium.

**WIN-8 | minor | 02 §2.3 H3, §2.7 E11, AGENTS.md keep-awake**
- **Claim:** Focus is lost to causes outside Praxion, and nothing keeps the Windows machine awake. AGENTS.md covers Amphetamine on the Mac only.
- **Evidence:**
  - GLFW `WM_KILLFOCUS`: the cursor is released, and a fullscreen window with auto-iconify is minimized.
  - MC `pauseIfInactive` opens the pause screen after 500 ms without focus.
  - `SendInput` cannot reach a locked screen, a screensaver or a UAC secure desktop. This is general Windows behaviour I did not verify here. UIPI sources cover the UAC case.
- **Recommendation:**
  - The body should call `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)` while active.
  - Add to the environment check: lock and sleep timers, notification suppression, pending updates.
  - Add to E11: a UAC prompt, a toast, lock and Alt-Tab, each with the "no stuck key and a clear message" criterion.
- **Confidence:** medium.

**WIN-9 | minor | 02 §2.3 H2 (menu mode), §2.7 E10**
- **Claim:** `MOUSEEVENTF_ABSOLUTE` coordinates are normalized and map to the primary monitor unless `MOUSEEVENTF_VIRTUALDESK` is set. E10's matrix (GUI scale 1-4, two window sizes) does not include a second monitor or mixed DPI.
- **Evidence:** MOUSEINPUT remarks: coordinates 0-65535 "map to the primary monitor"; `VIRTUALDESK` maps to the whole desktop.
- **Recommendation:**
  - Position the menu cursor with `SetCursorPos` in per-monitor-v2 pixels, and use `SendInput` only for button down/up. Rounding errors in the normalization go away.
  - Verify that MC's cursor callback sees the position and that your own hook is not triggered.
  - Add multi-monitor and mixed-DPI to E10.
- **Confidence:** medium.

**WIN-10 | minor | 02 §2.3 H1 ("bindings read from `options.txt`")**
- **Claim:** The mapping from binding to scan code is not specified.
- **Evidence:**
  - KEYBDINPUT docs: scan codes are layout-independent, while virtual keys change with layout.
  - `options.txt` holds GLFW key names. The 26.2 `KeyboardHandler` works on `event.key()`, not the scan code.
  - GLFW's `WM_KEYDOWN` handler builds the key from the scan code via its own table. Which table MC uses for bindings is unverified.
  - Text entry is a separate path. `KEYEVENTF_UNICODE` goes through `VK_PACKET`, then `WM_CHAR`. GLFW has no `VK_PACKET` handling and relies on `WM_CHAR`. Whether MC's chat accepts these is untested.
- **Recommendation:**
  - Use the inverse of GLFW's own key table (GLFW key to scan code), never `MapVirtualKey`.
  - Test with a Turkish-Q and a US layout.
  - Settle in a decision whether Praxion ever types chat. Turkish characters need the Unicode path, and user take-over mid-string would garble the message.
- **Confidence:** medium.

**WIN-11 | minor | 02 §2.7 E13, §2.6 timing**
- **Claim:** E13 ("p99 < 2 ms of the 50 ms loop") on a clean machine will not represent the user's machine.
- **Evidence:**
  - Hook calls are synchronous with the injector (see WIN-2). Every other global hook on the PC (vendor mouse software, overlays, antivirus) adds to each `SendInput`.
  - The camera is applied once per render frame (`handleAccumulatedMovement` in `runTick`). Bursts of counts collapse into one frame, so smoothness is capped by frame rate no matter how steady the injector is.
- **Recommendation:**
  - Run E13 on the real user machine with its normal software.
  - Put the injector on its own high-priority thread. Prefer MMCSS if it helps (untested).
  - Log QPC timestamps of each `SendInput` and compare with the proxy.
  - Spread camera counts across frame times in the style layer.
- **Confidence:** medium.

**WIN-12 | minor | 02 §2.7 "Hands experiments run at the physical Windows PC", AGENTS.md**
- **Claim:** The experiments assume a person at the PC. The production setup (user on macOS, "Windows PC dedicated to the game") likely involves remote use. The docs never say how the user takes over.
- **Evidence:**
  - RDPMouse mod page (search snippet): Minecraft's raw relative input is blocked over Remote Desktop.
  - A disconnected RDP session does not keep a rendering desktop (general knowledge, unverified here).
  - `SendInput` only reaches the foreground window of the interactive desktop (SendInput docs). Praxion therefore blocks all other use of the PC while it plays.
- **Recommendation:**
  - Write down the supported way the user connects and takes over.
  - Run E12 through that route.
  - Document that Praxion monopolizes the foreground.
- **Confidence:** medium.

**WIN-13 | minor | 02 §2.7 coverage of E5-E9, E11, E12**
- **Claim:** The experiments miss several risk points above.
- **Recommendation:** Add these experiments, or consciously drop them.
  - Sticky/Filter Keys with injected input (WIN-4).
  - A silently removed hook (WIN-2).
  - `ExtraInformation` and `hDevice` pass-through in raw input (WIN-2).
  - An elevated foreground window (WIN-3).
  - A remote injector (WIN-1).
  - A mouse-button hold across a watchdog release (WIN-5).
  - Keyboard layouts (WIN-10).
  - Frame-rate dependence of E7 and E8 (WIN-6).
  - The pointer-speed matrix (WIN-7).
- **Confidence:** high.

### Ideas

1. **Tag-based ownership:** a nonce in `dwExtraInfo` on every injected event, and treat anything else as the user. This is one line of design that removes the hook-vs-driver ambiguity.
2. **Probe canary:** an unbound key injected periodically, so the hook (and optionally the game's receipt of input) is proven alive.
3. **Tick-synchronous input scheduler:** the proxy's pose packets as the clock for holds and clicks, so the input layer never guesses client tick phase.
4. **Input tape:** log every injected event with QPC time and tag, then align with proxy packets. It serves E9 latency, stuck-key forensics and E12.
5. **`SetCursorPos` for menus,** with `SendInput` for buttons only.
6. **Environment doctor at start:** read-only checks of integrity levels, Sticky/Filter/Toggle Keys, pointer speed and EPP, `rawMouseInput`, layout, DPI, multi-monitor, lock/sleep, with a refusal message and a fix hint.
7. **Dead-man switch:** a tiny watchdog that waits on the body's process handle and replays the shared held-key table.
8. **Windows execution state:** `SetThreadExecutionState` while active, to replace the Mac-only Amphetamine step.

### Questions for the user

1. Do you ever control or watch the Windows PC remotely from the Mac (Parsec, Sunshine/Moonlight, AnyDesk, RDP)? That decides whether the injected flag can be the take-over signal (WIN-1, WIN-12).
2. Will Praxion type into Minecraft chat (Turkish replies), or only report through its own interface window (WIN-10)?
3. If the Sticky Keys test (WIN-4) is positive, is "refuse to start and tell the user to turn the shortcut off" acceptable, instead of Praxion changing the setting?
4. Is it acceptable that the PC is unusable for anything else while Praxion plays, since input needs the foreground (WIN-12)?
5. If closed-loop E7 still misses 95% at low frame rates, do you want to relax the criterion or require a minimum frame rate?

### Sources

Read directly:
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-keybdinput
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-kbdllhookstruct
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-msllhookstruct
- https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
- https://learn.microsoft.com/en-us/windows/win32/winmsg/about-hooks
- https://learn.microsoft.com/en-us/windows/win32/inputdev/using-raw-input
- https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-input
- https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-securityoverview
- https://raw.githubusercontent.com/glfw/glfw/master/src/win32_window.c
- https://raw.githubusercontent.com/glfw/glfw/master/src/input.c
- https://raw.githubusercontent.com/glfw/glfw/master/src/window.c
- https://raw.githubusercontent.com/Renekovski/26.2-mcp/main/src/net/minecraft/client/MouseHandler.java
- https://raw.githubusercontent.com/Renekovski/26.2-mcp/main/src/net/minecraft/client/KeyboardHandler.java
- https://raw.githubusercontent.com/Renekovski/26.2-mcp/main/src/net/minecraft/client/Minecraft.java (first 100,000 of 117,588 characters)
- https://github.com/jtroo/kanata/blob/main/src/oskbd/windows/llhook.rs

Search results only, not opened and not fully verified:
- https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-stickykeys
- https://discussions.unity.com/t/windows-sticky-keys-prompt/743349
- https://bugs-legacy.mojang.com/browse/MC-230466
- https://modrinth.com/mod/rdpmouse
- https://chromium.googlesource.com/chromium/src/+/4be952b08/remoting/host/input_injector_win.cc
- https://www.autohotkey.com/boards/viewtopic.php?p=453379
- https://github.com/P4suta/keep-pressing/wiki/Troubleshooting
- https://deadlydog.github.io/Prevent-Admin-apps-from-blocking-AutoHotkey-by-using-UI-Access/

Failed fetches, so nothing from them was used: Sunshine `input.cpp` (the file fetched was the gamepad/touch backend, with no keyboard or mouse code), the Wine commit page (HTTP 403) and the Stack Overflow thread on detecting software-generated input (fetch blocked).