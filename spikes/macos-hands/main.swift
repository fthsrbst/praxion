// macOS vanilla-Minecraft one-key hands spike.
//
// Scope: throwaway spike code, not Demo-1 product code. It proves one thing only:
// a tagged synthetic key can be posted to a specific Minecraft JVM, and a
// listen-only Quartz session tap can take authority back (key-up, at most once)
// on physical input, focus loss or a disabled tap.
//
// Safety model, in order of authority:
// 1. Nothing is posted until both preflight booleans are true, the target PID is
//    alive, it looks like a Minecraft/JVM process and it is the frontmost app.
// 2. The tap is listen-only and starts before the key-down. It rejects our own
//    events by tag and never records or logs input contents.
// 3. TakeoverMachine is the only decision point. It emits postKeyDown at most
//    once, postKeyUp at most once, never a second key-down, and cannot re-arm.
// 4. All state changes happen on the main thread: the tap callback, the
//    activation notification and the signal sources only set `pendingStop`; the
//    run-loop timer consumes it. That keeps posting and releasing serialized.
// 5. The hold deadline is 1.5 s while the run loop is responsive.
//
// Build and run: see README.md.

import AppKit
import CoreGraphics
import Darwin
import Foundation

// MARK: - Tunables

private enum Tunables {
    /// Written into every event we post; the tap ignores events carrying it.
    /// `kCGEventSourceUserData` is a hint field SDL/GLFW do not read on macOS.
    /// Bit 0x80 stays clear and the value is far from SDL's Windows 0xFF5157xx range.
    static let eventTag: Int64 = 0x5052_4148  // "PRAH"
    /// Long enough for a person to interrupt during the live test.
    static let holdNanoseconds: UInt64 = 1_500_000_000
    /// How long the user has to bring the test client to the foreground.
    static let focusWaitNanoseconds: UInt64 = 10_000_000_000
    /// Bound on the armed state before the key-down; guards an unbounded arming.
    static let armWindowNanoseconds: UInt64 = 400_000_000
    /// Run-loop polling interval for focus, tap state and release.
    static let tickNanoseconds: UInt64 = 10_000_000
    /// Last-resort bound after the hold deadline; should never be reached.
    static let hardStopSlackNanoseconds: UInt64 = 5_000_000_000
    static let minKeyCode = 0
    static let maxKeyCode = 127
}

// MARK: - Monotonic clock

private func monotonicNanos() -> UInt64 {
    DispatchTime.now().uptimeNanoseconds
}

// MARK: - Takeover state machine (pure, headless)

/// Why the spike stopped holding authority. Raw strings are the report values.
enum StopReason: String, Equatable {
    case holdCompleted = "hold_completed"
    case foreignInput = "foreign_input"
    case focusLost = "focus_lost"
    case tapDisabled = "tap_disabled"
    case armDeadline = "arm_deadline"
    case signal = "signal"
    case focusNotConfirmed = "focus_not_confirmed"
    case postFailed = "post_failed"
    case hardStop = "hard_stop"
}

enum MachineEvent: Equatable {
    case arm
    /// `focusConfirmed` is the result of a focus check taken immediately before
    /// the post, not a cached value.
    case beginHold(focusConfirmed: Bool)
    case foreignInput
    case focusLost
    case tapDisabled
    case armDeadline
    case holdElapsed
    case signal
    case postFailed
    case hardStop
}

enum MachineAction: Equatable {
    case postKeyDown
    case postKeyUp
}

/// The whole takeover decision, as one small value type so it can be tested
/// without permissions, a window server or a game.
struct TakeoverMachine {
    enum Phase: Equatable {
        case idle
        case armed      // may post the key-down; nothing is held yet
        case holding    // key-down posted, key-up not yet posted
        case finished   // terminal; no re-arm, no further posts
    }

    private(set) var phase: Phase = .idle
    private(set) var downPosts = 0
    private(set) var upPosts = 0
    private(set) var stopReason: StopReason?

    /// The safety invariant the report and the self-test both rely on.
    var invariantHolds: Bool {
        guard downPosts <= 1, upPosts <= 1, upPosts <= downPosts else { return false }
        switch phase {
        case .idle:
            return downPosts == 0 && upPosts == 0 && stopReason == nil
        case .armed:
            return downPosts == 0 && upPosts == 0 && stopReason == nil
        case .holding:
            return downPosts == 1 && upPosts == 0 && stopReason == nil
        case .finished:
            return stopReason != nil
        }
    }

    mutating func handle(_ event: MachineEvent) -> [MachineAction] {
        switch event {
        case .arm:
            guard phase == .idle else { return [] }
            phase = .armed
            return []

        case .beginHold(let focusConfirmed):
            guard phase == .armed else { return [] }
            guard focusConfirmed else {
                finish(.focusNotConfirmed)
                return []
            }
            phase = .holding
            downPosts += 1
            return [.postKeyDown]

        case .holdElapsed:
            guard phase == .holding else { return [] }
            finish(.holdCompleted)
            upPosts += 1
            return [.postKeyUp]

        case .armDeadline:
            guard phase == .armed else { return [] }
            finish(.armDeadline)
            return []

        case .foreignInput:
            return stop(.foreignInput)
        case .focusLost:
            return stop(.focusLost)
        case .tapDisabled:
            return stop(.tapDisabled)
        case .signal:
            return stop(.signal)
        case .hardStop:
            return stop(.hardStop)
        case .postFailed:
            return stop(.postFailed)
        }
    }

    /// Stops from either live phase. A key-up is emitted only when a key-down was
    /// actually posted, and only one event can fail into this path per run.
    private mutating func stop(_ reason: StopReason) -> [MachineAction] {
        switch phase {
        case .armed:
            finish(reason)
            return []
        case .holding:
            finish(reason)
            upPosts += 1
            return [.postKeyUp]
        case .idle, .finished:
            return []
        }
    }

    private mutating func finish(_ reason: StopReason) {
        phase = .finished
        if stopReason == nil { stopReason = reason }
    }
}

// MARK: - Command line

enum Mode: Equatable {
    case help
    case preflight
    case selfTest
    case run
}

struct Options: Equatable {
    let mode: Mode
    let pid: pid_t
    let keyCode: CGKeyCode
}

struct ArgumentError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

private let usageText = """
usage:
  macos-hands --preflight
  macos-hands --self-test
  macos-hands --run --pid <pid> --keycode <0...127>

modes (exactly one):
  --preflight   print Quartz post and listen permission booleans; never prompts
  --self-test   headless state-machine and argument tests; needs no permission
  --run         live one-key hold against an already running test client

run options:
  --pid PID     running Minecraft (Java) process id
  --keycode K   CoreGraphics virtual key code; ANSI W is 13

exit codes: 0 ok, 2 refused (arguments, permissions, target), 3 aborted,
            4 failed (an event could not be posted)
"""

private func fail(_ message: String) -> Result<Options, ArgumentError> {
    .failure(ArgumentError(message))
}

/// Pure parser: no lookups, no permissions, so the self-test exercises it directly.
func parseArguments(_ args: [String]) -> Result<Options, ArgumentError> {
    if args.isEmpty {
        return fail("no mode given")
    }
    // Explicit help wins over anything else, so the user can always see usage.
    if args.contains("--help") {
        return .success(Options(mode: .help, pid: 0, keyCode: 0))
    }

    var mode: Mode?
    var pid: pid_t?
    var keyCode: CGKeyCode?
    var index = 0

    func nextValue(for flag: String) -> String? {
        let valueIndex = index + 1
        guard valueIndex < args.count else { return nil }
        return args[valueIndex]
    }

    while index < args.count {
        let token = args[index]
        switch token {
        case "--preflight", "--self-test", "--run":
            guard mode == nil else {
                return fail("more than one mode flag")
            }
            switch token {
            case "--preflight": mode = .preflight
            case "--self-test": mode = .selfTest
            default: mode = .run
            }
            index += 1

        case "--pid":
            guard pid == nil else { return fail("--pid given twice") }
            guard let raw = nextValue(for: token) else { return fail("--pid needs a value") }
            guard isDecimal(raw), raw.count <= 10, let parsed = Int32(raw), parsed > 1 else {
                return fail("--pid must be a decimal process id above 1")
            }
            pid = parsed
            index += 2

        case "--keycode":
            guard keyCode == nil else { return fail("--keycode given twice") }
            guard let raw = nextValue(for: token) else { return fail("--keycode needs a value") }
            guard isDecimal(raw), raw.count <= 3, let parsed = Int(raw), parsed >= Tunables.minKeyCode,
                  parsed <= Tunables.maxKeyCode
            else {
                return fail("--keycode must be \(Tunables.minKeyCode)...\(Tunables.maxKeyCode)")
            }
            keyCode = CGKeyCode(parsed)
            index += 2

        default:
            let shown = token.count > 24 ? String(token.prefix(24)) + "..." : token
            return fail("unknown argument '\(shown)'")
        }
    }

    guard let mode else { return fail("no mode given") }

    switch mode {
    case .help:
        return .success(Options(mode: .help, pid: 0, keyCode: 0))
    case .preflight, .selfTest:
        guard pid == nil else { return fail("--pid is only valid with --run") }
        guard keyCode == nil else { return fail("--keycode is only valid with --run") }
        return .success(Options(mode: mode, pid: 0, keyCode: 0))
    case .run:
        guard let pid else { return fail("--run needs --pid") }
        guard let keyCode else { return fail("--run needs --keycode") }
        guard !isModifierKeyCode(keyCode) else {
            return fail("--keycode \(keyCode) is a modifier; a held modifier can trigger shortcuts, refusing")
        }
        return .success(Options(mode: .run, pid: pid, keyCode: keyCode))
    }
}

private func isDecimal(_ text: String) -> Bool {
    !text.isEmpty && text.allSatisfy { $0.isASCII && $0.isWholeNumber }
}

/// kVK_Command..kVK_RightControl (54...63), including caps lock and both sides.
/// Posting a held modifier to another app is never needed for this test.
private func isModifierKeyCode(_ keyCode: CGKeyCode) -> Bool {
    (54...63).contains(Int(keyCode))
}

// MARK: - Permissions

struct Preflight {
    let postEvents: Bool
    let listenEvents: Bool

    var ok: Bool { postEvents && listenEvents }

    /// Never calls CGRequest*; this spike must not change TCC state.
    static func read() -> Preflight {
        Preflight(postEvents: CGPreflightPostEventAccess(), listenEvents: CGPreflightListenEventAccess())
    }
}

func runPreflight() -> Int32 {
    let preflight = Preflight.read()
    print("mode=preflight")
    print("post_event_access=\(preflight.postEvents)")
    print("listen_event_access=\(preflight.listenEvents)")
    if preflight.ok {
        print("result=ok")
        return 0
    }
    print("result=fail")
    print("hint=grant Accessibility (post) and Input Monitoring (listen) to the terminal app; no prompt was shown")
    return 2
}

// MARK: - Self-test

struct ScenarioStep {
    let event: MachineEvent
    let actions: [MachineAction]
    let phase: TakeoverMachine.Phase
}

func runSelfTest() -> Int32 {
    print("mode=self-test")

    var checks = 0
    var failures = 0

    func check(_ name: String, _ condition: Bool) {
        checks += 1
        if !condition {
            failures += 1
            print("fail \(name)")
        }
    }

    func scenario(
        _ name: String,
        _ steps: [ScenarioStep],
        reason: StopReason?,
        down: Int,
        up: Int
    ) {
        var machine = TakeoverMachine()
        for (stepIndex, step) in steps.enumerated() {
            let actions = machine.handle(step.event)
            check("\(name)/step\(stepIndex)/actions", actions == step.actions)
            check("\(name)/step\(stepIndex)/phase", machine.phase == step.phase)
            check("\(name)/step\(stepIndex)/invariant", machine.invariantHolds)
            check("\(name)/step\(stepIndex)/down_posts", machine.downPosts <= 1)
            check("\(name)/step\(stepIndex)/up_posts", machine.upPosts <= 1)
        }
        check("\(name)/reason", machine.stopReason == reason)
        check("\(name)/down_total", machine.downPosts == down)
        check("\(name)/up_total", machine.upPosts == up)
        print("scenario \(name) reason=\(reason?.rawValue ?? "none") down=\(down) up=\(up)")
    }

    scenario("hold_completes", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .holdElapsed, actions: [.postKeyUp], phase: .finished),
    ], reason: .holdCompleted, down: 1, up: 1)

    scenario("foreign_input_while_holding", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .foreignInput, actions: [.postKeyUp], phase: .finished),
    ], reason: .foreignInput, down: 1, up: 1)

    scenario("foreign_input_while_armed_posts_nothing", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .foreignInput, actions: [], phase: .finished),
    ], reason: .foreignInput, down: 0, up: 0)

    scenario("focus_not_confirmed_never_posts", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: false), actions: [], phase: .finished),
    ], reason: .focusNotConfirmed, down: 0, up: 0)

    scenario("focus_lost_while_holding", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .focusLost, actions: [.postKeyUp], phase: .finished),
    ], reason: .focusLost, down: 1, up: 1)

    scenario("tap_disabled_while_holding", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .tapDisabled, actions: [.postKeyUp], phase: .finished),
    ], reason: .tapDisabled, down: 1, up: 1)

    scenario("arm_deadline_before_down", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .armDeadline, actions: [], phase: .finished),
    ], reason: .armDeadline, down: 0, up: 0)

    scenario("signal_while_armed", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .signal, actions: [], phase: .finished),
    ], reason: .signal, down: 0, up: 0)

    scenario("signal_while_holding_releases_once", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .signal, actions: [.postKeyUp], phase: .finished),
    ], reason: .signal, down: 1, up: 1)

    scenario("hard_stop_while_holding", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .hardStop, actions: [.postKeyUp], phase: .finished),
    ], reason: .hardStop, down: 1, up: 1)

    scenario("post_failed_while_armed", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .postFailed, actions: [], phase: .finished),
    ], reason: .postFailed, down: 0, up: 0)

    scenario("no_rearm_after_finish", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .holdElapsed, actions: [.postKeyUp], phase: .finished),
        ScenarioStep(event: .arm, actions: [], phase: .finished),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [], phase: .finished),
        ScenarioStep(event: .foreignInput, actions: [], phase: .finished),
        ScenarioStep(event: .armDeadline, actions: [], phase: .finished),
        ScenarioStep(event: .holdElapsed, actions: [], phase: .finished),
    ], reason: .holdCompleted, down: 1, up: 1)

    scenario("late_events_after_release_send_nothing", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .foreignInput, actions: [.postKeyUp], phase: .finished),
        ScenarioStep(event: .focusLost, actions: [], phase: .finished),
        ScenarioStep(event: .tapDisabled, actions: [], phase: .finished),
        ScenarioStep(event: .signal, actions: [], phase: .finished),
    ], reason: .foreignInput, down: 1, up: 1)

    scenario("hold_elapsed_while_armed_is_ignored", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .holdElapsed, actions: [], phase: .armed),
    ], reason: nil, down: 0, up: 0)

    scenario("arm_deadline_while_holding_keeps_key_state", [
        ScenarioStep(event: .arm, actions: [], phase: .armed),
        ScenarioStep(event: .beginHold(focusConfirmed: true), actions: [.postKeyDown], phase: .holding),
        ScenarioStep(event: .armDeadline, actions: [], phase: .holding),
    ], reason: nil, down: 1, up: 0)

    // Argument parser: pure, so every rejection path is tested here.
    func argument(_ name: String, _ args: [String], success: Mode?) {
        let result = parseArguments(args)
        switch (result, success) {
        case (.success(let options), .some(let expected)):
            check("args/\(name)/mode", options.mode == expected)
        case (.failure, .none):
            break
        default:
            check("args/\(name)", false)
        }
    }

    argument("help", ["--help"], success: .help)
    argument("preflight", ["--preflight"], success: .preflight)
    argument("self_test", ["--self-test"], success: .selfTest)
    argument("run", ["--run", "--pid", "4242", "--keycode", "13"], success: .run)
    argument("keycode_bounds", ["--run", "--pid", "4242", "--keycode", "127"], success: .run)
    argument("keycode_zero", ["--run", "--pid", "4242", "--keycode", "0"], success: .run)
    argument("no_mode", [], success: nil)
    argument("two_modes", ["--run", "--preflight"], success: nil)
    argument("missing_pid", ["--run", "--keycode", "13"], success: nil)
    argument("missing_keycode", ["--run", "--pid", "4242"], success: nil)
    argument("pid_zero", ["--run", "--pid", "0", "--keycode", "13"], success: nil)
    argument("pid_one", ["--run", "--pid", "1", "--keycode", "13"], success: nil)
    argument("pid_negative", ["--run", "--pid", "-5", "--keycode", "13"], success: nil)
    argument("pid_not_decimal", ["--run", "--pid", "12a", "--keycode", "13"], success: nil)
    argument("pid_overflow", ["--run", "--pid", "2147483648", "--keycode", "13"], success: nil)
    argument("keycode_128", ["--run", "--pid", "4242", "--keycode", "128"], success: nil)
    argument("keycode_negative", ["--run", "--pid", "4242", "--keycode", "-1"], success: nil)
    argument("keycode_float", ["--run", "--pid", "4242", "--keycode", "1.3"], success: nil)
    argument("keycode_missing_value", ["--run", "--pid", "4242", "--keycode"], success: nil)
    argument("keycode_modifier", ["--run", "--pid", "4242", "--keycode", "55"], success: nil)
    argument("duplicate_pid", ["--run", "--pid", "4242", "--pid", "4243", "--keycode", "13"], success: nil)
    argument("unknown_flag", ["--preflight", "--hold", "5"], success: nil)
    argument("pid_with_preflight", ["--preflight", "--pid", "4242"], success: nil)
    argument("keycode_with_self_test", ["--self-test", "--keycode", "13"], success: nil)
    argument("extra_positional", ["--preflight", "13"], success: nil)
    argument("help_wins", ["--preflight", "--help"], success: .help)

    if let parsed = try? parseArguments(["--run", "--pid", "4242", "--keycode", "13"]).get() {
        check("args/run/pid_value", parsed.pid == 4242)
        check("args/run/keycode_value", parsed.keyCode == 13)
    } else {
        check("args/run/parses", false)
    }

    print("self_test_checks=\(checks) self_test_failures=\(failures)")
    if failures == 0 {
        print("result=ok")
        return 0
    }
    print("result=fail")
    return 1
}

// MARK: - Live run

/// Main-thread object. Every mutation happens on the main run loop, so the tap
/// callback, the activation notification and the signal sources are reduced to
/// setting `pendingStop`, which the timer consumes.
final class HandsRunner {
    private let options: Options
    private var machine = TakeoverMachine()

    private var eventSource: CGEventSource?
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var timer: Timer?
    private var activationObserver: NSObjectProtocol?
    private var signalSources: [DispatchSourceSignal] = []

    private var pendingStop: StopReason?
    private var receivedSignal: Int32?

    private var armDeadline: UInt64 = 0
    private var holdDeadline: UInt64 = 0
    private var hardDeadline: UInt64 = 0
    private var downPostedAt: UInt64 = 0
    private var upPostedAt: UInt64 = 0
    private var foreignObservedAt: UInt64?

    private var didArm = false
    private var focusConfirmedBeforeDown = false
    private var keyDownPosted = false
    private var keyUpPosted = false
    private var keyUpFailed = false
    private var foreignEvents = 0
    private var selfEventsIgnored = 0
    private var tapDisabledEvents = 0
    private var startedAt = monotonicNanos()

    init(options: Options) {
        self.options = options
    }

    // MARK: Entry

    func run() -> Int32 {
        startedAt = monotonicNanos()
        print("mode=live")
        print("pid=\(options.pid)")
        print("keycode=\(options.keyCode)")
        print("hold_ms=\(Tunables.holdNanoseconds / 1_000_000)")
        print("note=posted_events_do_not_prove_sdl_or_game_accepted_them")

        let preflight = Preflight.read()
        print("preflight_post_event_access=\(preflight.postEvents)")
        print("preflight_listen_event_access=\(preflight.listenEvents)")
        guard preflight.ok else {
            return refused("missing_preflight")
        }

        guard options.pid != getpid() else {
            return refused("target_is_self")
        }
        guard let executablePath = targetExecutablePath(options.pid) else {
            return refused("target_unavailable")
        }
        print("target_executable=\((executablePath as NSString).lastPathComponent)")
        guard isTestClient(executablePath: executablePath) else {
            return refused("target_not_minecraft")
        }
        if let app = NSRunningApplication(processIdentifier: options.pid) {
            print("target_name=\(app.localizedName ?? "unknown")")
        } else {
            print("target_name=unknown")
        }

        installSignalHandlers()

        guard waitForForeground() else {
            return abortedBeforeArm(pendingStop == .signal ? "signal" : "focus_not_foreground")
        }

        guard let source = CGEventSource(stateID: .hidSystemState) else {
            return refused("event_source_unavailable")
        }
        eventSource = source

        guard createListenTap() else {
            return refused("event_tap_unavailable")
        }
        print("tap_created=true")

        addActivationObserver()

        armDeadline = monotonicNanos() + Tunables.armWindowNanoseconds
        apply(.arm)
        guard machine.phase == .armed else {
            return refused("arm_rejected")
        }
        didArm = true
        print("armed=true")

        let timer = Timer(timeInterval: Double(Tunables.tickNanoseconds) / 1_000_000_000, repeats: true) {
            [weak self] _ in
            self?.tick()
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer

        // RunLoop.run() restarts after CFRunLoopStop while sources remain installed.
        CFRunLoopRun()

        return finishRun()
    }

    // MARK: Preconditions

    private func targetExecutablePath(_ pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: 4096)
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        let bytes = buffer.prefix(Int(length)).map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    /// Coarse executable check. The caller must supply the test client's PID.
    private func isTestClient(executablePath: String) -> Bool {
        let lowercased = executablePath.lowercased()
        let name = (lowercased as NSString).lastPathComponent
        return name == "java" || lowercased.contains("/minecraft") || lowercased.contains("/runtime/java")
    }

    private func isTargetForeground() -> Bool {
        guard let front = NSWorkspace.shared.frontmostApplication else { return false }
        guard front.processIdentifier == options.pid else { return false }
        return front.isActive
    }

    private func waitForForeground() -> Bool {
        let deadline = monotonicNanos() + Tunables.focusWaitNanoseconds
        while monotonicNanos() < deadline {
            if pendingStop != nil { return false }
            if isTargetForeground() { return true }
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        return isTargetForeground()
    }

    // MARK: Tap

    private func createListenTap() -> Bool {
        let mask: CGEventMask =
            (CGEventMask(1) << CGEventMask(CGEventType.keyDown.rawValue))
            | (CGEventMask(1) << CGEventMask(CGEventType.flagsChanged.rawValue))
            | (CGEventMask(1) << CGEventMask(CGEventType.leftMouseDown.rawValue))
            | (CGEventMask(1) << CGEventMask(CGEventType.rightMouseDown.rawValue))
            | (CGEventMask(1) << CGEventMask(CGEventType.otherMouseDown.rawValue))

        guard
            let tap = CGEvent.tapCreate(
                tap: .cgSessionEventTap,
                place: .tailAppendEventTap,
                options: .listenOnly,
                eventsOfInterest: mask,
                callback: handsTapCallback,
                userInfo: Unmanaged.passUnretained(self).toOpaque()
            )
        else {
            return false
        }
        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0) else {
            CFMachPortInvalidate(tap)
            return false
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        eventTap = tap
        runLoopSource = source
        return true
    }

    func handleTap(type: CGEventType, event: CGEvent) {
        precondition(Thread.isMainThread, "tap callback must stay on the main run loop")

        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            tapDisabledEvents += 1
            requestStop(.tapDisabled)
            return
        }

        switch type {
        case .keyDown, .flagsChanged, .leftMouseDown, .rightMouseDown, .otherMouseDown:
            break
        default:
            return
        }

        // Our own events carry the tag; nothing else is read from the event.
        if event.getIntegerValueField(.eventSourceUserData) == Tunables.eventTag {
            selfEventsIgnored += 1
            return
        }

        guard machine.phase == .armed || machine.phase == .holding else { return }
        foreignEvents += 1
        if foreignObservedAt == nil { foreignObservedAt = monotonicNanos() }
        requestStop(.foreignInput)
    }

    private func addActivationObserver() {
        activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            guard self.machine.phase == .armed || self.machine.phase == .holding else { return }
            if !self.isTargetForeground() {
                self.requestStop(.focusLost)
            }
        }
    }

    // MARK: Signals

    private func installSignalHandlers() {
        for sig in [SIGINT, SIGTERM] {
            signal(sig, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            source.setEventHandler { [weak self] in
                guard let self else { return }
                self.receivedSignal = sig
                self.requestStop(.signal)
            }
            source.resume()
            signalSources.append(source)
        }
    }

    /// Called from the tap callback, the activation observer and the signal
    /// sources. Only records intent; the timer applies it, so posting stays
    /// serialized and no event is posted from inside a callback.
    private func requestStop(_ reason: StopReason) {
        if pendingStop == nil { pendingStop = reason }
    }

    // MARK: Tick

    private func tick() {
        switch machine.phase {
        case .idle, .finished:
            return
        case .armed, .holding:
            break
        }

        if let reason = pendingStop {
            pendingStop = nil
            apply(event(for: reason))
            return
        }

        guard let eventTap, CGEvent.tapIsEnabled(tap: eventTap) else {
            apply(.tapDisabled)
            return
        }

        switch machine.phase {
        case .armed:
            if CGEventSource.keyState(.hidSystemState, key: options.keyCode) {
                apply(.foreignInput)
                return
            }
            if !isTargetForeground() {
                apply(.focusLost)
                return
            }
            if monotonicNanos() >= armDeadline {
                apply(.armDeadline)
                return
            }
            // The focus answer is taken at this instant, not cached: this is the
            // check that immediately precedes the post.
            let confirmed = isTargetForeground()
            focusConfirmedBeforeDown = confirmed
            apply(.beginHold(focusConfirmed: confirmed))

        case .holding:
            if !isTargetForeground() {
                apply(.focusLost)
                return
            }
            if monotonicNanos() >= hardDeadline {
                apply(.hardStop)
                return
            }
            if monotonicNanos() >= holdDeadline {
                apply(.holdElapsed)
            }

        case .idle, .finished:
            break
        }
    }

    private func event(for reason: StopReason) -> MachineEvent {
        switch reason {
        case .foreignInput: return .foreignInput
        case .focusLost: return .focusLost
        case .tapDisabled: return .tapDisabled
        case .signal: return .signal
        case .hardStop: return .hardStop
        case .holdCompleted, .armDeadline, .focusNotConfirmed, .postFailed:
            // These reasons are produced by the machine itself, never queued.
            return .hardStop
        }
    }

    /// Runs the machine and performs only the actions it emits.
    private func apply(_ event: MachineEvent) {
        var actions = machine.handle(event)
        while !actions.isEmpty {
            let action = actions.removeFirst()
            switch action {
            case .postKeyDown:
                if postKey(down: true) {
                    keyDownPosted = true
                    downPostedAt = monotonicNanos()
                    holdDeadline = downPostedAt + Tunables.holdNanoseconds
                    hardDeadline = holdDeadline + Tunables.hardStopSlackNanoseconds
                } else {
                    actions.append(contentsOf: machine.handle(.postFailed))
                }

            case .postKeyUp:
                guard keyDownPosted else { continue }
                if postKey(down: false) {
                    keyUpPosted = true
                    upPostedAt = monotonicNanos()
                } else {
                    keyUpFailed = true
                }
            }
        }

        if machine.phase == .finished {
            CFRunLoopStop(CFRunLoopGetMain())
        }
    }

    private func postKey(down: Bool) -> Bool {
        guard let eventSource else { return false }
        guard
            let event = CGEvent(
                keyboardEventSource: eventSource,
                virtualKey: options.keyCode,
                keyDown: down
            )
        else {
            return false
        }
        event.setIntegerValueField(.eventSourceUserData, value: Tunables.eventTag)
        event.flags = CGEventFlags()
        if down && !isTargetForeground() { return false }
        event.postToPid(options.pid)
        return true
    }

    // MARK: Teardown and report

    private func finishRun() -> Int32 {
        if timer != nil {
            timer?.invalidate()
            timer = nil
        }
        if let tap = eventTap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let source = runLoopSource {
                CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
            }
            CFMachPortInvalidate(tap)
            eventTap = nil
            runLoopSource = nil
        }
        if let observer = activationObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(observer)
            activationObserver = nil
        }
        for source in signalSources {
            source.cancel()
        }
        signalSources.removeAll()

        if !machine.invariantHolds {
            keyUpFailed = true
        }

        let reason = machine.stopReason
        let heldMs = (keyUpPosted && keyDownPosted) ? (upPostedAt - downPostedAt) / 1_000_000 : 0

        print("armed=\(didArm)")
        print("focus_before_down=\(focusConfirmedBeforeDown)")
        print("key_down_posted=\(keyDownPosted)")
        print("key_up_posted=\(keyUpPosted)")
        if keyUpFailed {
            print("key_up_failed=true")
        }
        if let receivedSignal {
            print("signal=\(receivedSignal)")
        }
        print("stop_reason=\(reason?.rawValue ?? "none")")
        print("hold_observed_ms=\(heldMs)")
        if reason == .foreignInput, keyUpPosted, let foreignObservedAt {
            print("tap_to_key_up_us=\((upPostedAt - foreignObservedAt) / 1_000)")
        }
        print("tap_foreign_events=\(foreignEvents)")
        print("tap_self_events_ignored=\(selfEventsIgnored)")
        print("tap_disabled_events=\(tapDisabledEvents)")
        print("elapsed_ms=\((monotonicNanos() - startedAt) / 1_000_000)")
        print("note=posted_events_do_not_prove_sdl_or_game_accepted_them")

        if keyDownPosted && !keyUpPosted {
            print("result=failed")
            print("detail=key_down_was_posted_without_a_key_up; the client may think the key is still held until it loses focus")
            return 4
        }
        if keyUpFailed || reason == .postFailed {
            print("result=failed")
            return 4
        }
        switch reason {
        case .holdCompleted:
            print("result=ok")
            return 0
        case .none:
            print("result=aborted")
            print("detail=no_terminal_state")
            return 3
        default:
            print("result=aborted")
            return 3
        }
    }

    private func refused(_ reason: String) -> Int32 {
        print("result=refused")
        print("reason=\(reason)")
        print("key_down_posted=false")
        print("key_up_posted=false")
        print("detail=no_event_was_posted")
        return 2
    }

    private func abortedBeforeArm(_ reason: String) -> Int32 {
        print("armed=false")
        print("key_down_posted=false")
        print("key_up_posted=false")
        print("tap_created=false")
        print("result=aborted")
        print("reason=\(reason)")
        return 3
    }
}

/// C callback: no allocation, no logging, no posting; it only records intent.
private func handsTapCallback(
    proxy: CGEventTapProxy,
    type: CGEventType,
    event: CGEvent,
    refcon: UnsafeMutableRawPointer?
) -> Unmanaged<CGEvent>? {
    if let refcon {
        let runner = Unmanaged<HandsRunner>.fromOpaque(refcon).takeUnretainedValue()
        runner.handleTap(type: type, event: event)
    }
    return Unmanaged.passUnretained(event)
}

// MARK: - Entry point

let arguments = Array(CommandLine.arguments.dropFirst())

switch parseArguments(arguments) {
case .failure(let error):
    print("error=\(error.description)")
    print(usageText)
    exit(2)
case .success(let options):
    switch options.mode {
    case .help:
        print(usageText)
        exit(0)
    case .preflight:
        exit(runPreflight())
    case .selfTest:
        exit(runSelfTest())
    case .run:
        exit(HandsRunner(options: options).run())
    }
}
