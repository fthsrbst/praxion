// praxion-hands: the macOS input helper of Praxion's real-client body.
//
// One process owns every OS input call. The body (Rust) writes one line per
// client tick to this process's stdin and reads one reply line from stdout.
//
// Safety model, in order of authority:
// 1. Nothing is posted until both preflight booleans are true, the target PID
//    is alive, it looks like a Minecraft/JVM process and it is frontmost.
// 2. Every posted event carries `eventSourceUserData`; the listen-only session
//    tap treats an untagged event as the operator taking over.
// 3. `HandsMachine` is the only decision point. It is pure: it takes one event
//    and returns the list of posts and lines to perform. It never posts twice
//    and never leaves a key or button held after a takeover or a terminal
//    transition.
// 4. All posting and all state changes happen on the main run loop. The stdin
//    reader thread, the tap callback, the activation observer and the signal
//    sources only record intent.
//
// Modes, protocol and safety rules: README.md in this directory.

import AppKit
import CoreGraphics
import Darwin
import Foundation

// MARK: - Tunables

enum Tunables {
    /// Written into every event we post; the tap ignores events carrying it.
    static let eventTag: Int64 = 0x5052_4148  // "PRAH"
    /// How long the target has to become frontmost before start-up is refused.
    static let focusWaitNanoseconds: UInt64 = 15_000_000_000
    /// No frame for this long while something is held releases everything.
    static let idleTimeoutNanoseconds: UInt64 = 250_000_000
    /// Run-loop polling interval: stdin drain, focus, tap health, idle.
    static let tickNanoseconds: UInt64 = 10_000_000
    /// Bound on one frame's look delta, in pixels.
    static let maxPixelsPerFrame = 600
    static let clickState: Int64 = 1
}

// MARK: - Monotonic clock

func monotonicNanos() -> UInt64 {
    DispatchTime.now().uptimeNanoseconds
}

// MARK: - Keys

/// One key the helper can press: its bit in the frame mask and its virtual code.
struct KeyBinding {
    let bit: UInt8
    let code: Int
}

/// Frame bit order. Releases and presses follow this order, so the action list
/// of a frame is fully determined.
let keyBindings: [KeyBinding] = [
    KeyBinding(bit: 1 << 0, code: 13),  // W, forward
    KeyBinding(bit: 1 << 1, code: 1),  // S, back
    KeyBinding(bit: 1 << 2, code: 0),  // A, left
    KeyBinding(bit: 1 << 3, code: 2),  // D, right
    KeyBinding(bit: 1 << 4, code: 49),  // Space, jump
]

/// ANSI number keys 1...9 on the top row, in slot order.
let slotKeyCodes: [Int] = [18, 19, 20, 21, 23, 22, 26, 28, 25]

// MARK: - Protocol types

enum MouseButton: Int, CaseIterable, Equatable {
    case left = 0
    case right = 1

    var mask: UInt8 { UInt8(1) << UInt8(rawValue) }
}

/// Event type of a look post, decided by what the helper holds at post time.
enum MoveKind: Equatable {
    case moved
    case leftDragged
    case rightDragged
}

/// What the state machine asks the OS runner to do. The self-test compares
/// these lists directly, so ordering inside a list is part of the contract.
enum Action: Equatable {
    case postKey(code: Int, down: Bool)
    case postButton(button: MouseButton, down: Bool)
    case postMove(dx: Int, dy: Int, kind: MoveKind)
    case printLine(String)
}

struct Frame: Equatable {
    let tick: UInt64
    let generation: UInt64
    let keys: UInt8
    let dx: Int
    let dy: Int
    let buttons: UInt8
    let slot: Int?
}

enum ProtocolLine: Equatable {
    case arm
    case disarm
    case frame(Frame)
    case malformed
}

enum TakeoverReason: String, Equatable {
    case foreignInput = "foreign_input"
    case focusLost = "focus_lost"
    case tapDisabled = "tap_disabled"
}

// MARK: - Reply lines

func readyLine() -> String { "READY" }
func okLine(tick: UInt64) -> String { "OK \(tick)" }
func refusedLine(tick: UInt64, reason: String) -> String { "REFUSED \(tick) \(reason)" }
func takeoverLine(_ reason: TakeoverReason) -> String { "TAKEOVER \(reason.rawValue)" }
func byeLine(reason: String) -> String { "BYE \(reason)" }
func idleLine() -> String { "IDLE" }

// MARK: - Line parser (pure)

private func parseUnsigned(_ text: Substring, maxDigits: Int, maxValue: UInt64) -> UInt64? {
    guard !text.isEmpty, text.count <= maxDigits,
          text.allSatisfy({ $0.isASCII && $0.isWholeNumber })
    else { return nil }
    guard let value = UInt64(text), value <= maxValue else { return nil }
    return value
}

private func parseSigned(_ text: Substring, maxMagnitude: Int) -> Int? {
    var body = text
    var negative = false
    if body.first == "-" {
        negative = true
        body = body.dropFirst()
    }
    guard !body.isEmpty, body.count <= 3,
          body.allSatisfy({ $0.isASCII && $0.isWholeNumber }),
          let magnitude = Int(body), magnitude <= maxMagnitude
    else { return nil }
    return negative ? -magnitude : magnitude
}

/// Fields are separated by exactly one space: a doubled, leading or trailing
/// space leaves an empty field and makes the line malformed.
func parseLine(_ line: String) -> ProtocolLine {
    // Splitting keeps empty fields, and an empty line yields one empty field.
    let fields = line.split(separator: " ", omittingEmptySubsequences: false)
    let command = fields[0]

    switch command {
    case "ARM":
        return fields.count == 1 ? .arm : .malformed

    case "DISARM":
        return fields.count == 1 ? .disarm : .malformed

    case "F":
        guard fields.count == 8 else { return .malformed }
        guard
            let tick = parseUnsigned(fields[1], maxDigits: 20, maxValue: UInt64.max),
            let generation = parseUnsigned(fields[2], maxDigits: 20, maxValue: UInt64.max),
            let keys = parseUnsigned(fields[3], maxDigits: 3, maxValue: 31),
            let dx = parseSigned(fields[4], maxMagnitude: Tunables.maxPixelsPerFrame),
            let dy = parseSigned(fields[5], maxMagnitude: Tunables.maxPixelsPerFrame),
            let buttons = parseUnsigned(fields[6], maxDigits: 1, maxValue: 3)
        else { return .malformed }

        let slotField = fields[7]
        let slot: Int?
        if slotField == "-" {
            slot = nil
        } else if slotField.count == 1,
                  slotField.allSatisfy({ $0.isASCII && $0.isWholeNumber }),
                  let value = Int(slotField), (1...9).contains(value) {
            slot = value
        } else {
            return .malformed
        }

        return .frame(
            Frame(
                tick: tick,
                generation: generation,
                keys: UInt8(keys),
                dx: dx,
                dy: dy,
                buttons: UInt8(buttons),
                slot: slot))

    default:
        return .malformed
    }
}

// MARK: - State machine (pure)

enum HandsEvent: Equatable {
    case arm
    case disarm
    /// The focus and cursor answers are taken immediately before the frame is
    /// applied, not cached.
    case frame(Frame, focusConfirmed: Bool, cursorOverTarget: Bool, nowNanos: UInt64)
    case foreignInput
    case focusLost
    case tapDisabled
    case idleCheck(nowNanos: UInt64)
    case endOfInput(reason: String)
}

/// The whole decision logic of the helper, with no OS call inside.
struct HandsMachine {
    enum Phase: Equatable {
        case notArmed
        case armed
    }

    private(set) var phase: Phase = .notArmed
    private(set) var heldKeys: Set<Int> = []
    private(set) var heldButtons: Set<MouseButton> = []
    private(set) var highestGeneration: UInt64 = 0
    private(set) var lastFrameNanos: UInt64?

    var isArmed: Bool { phase == .armed }
    var isHolding: Bool { !heldKeys.isEmpty || !heldButtons.isEmpty }

    /// Whether applying this frame needs the topmost-window check.
    func frameRequestsButtonDown(_ frame: Frame) -> Bool {
        MouseButton.allCases.contains { button in
            frame.buttons & button.mask != 0 && !heldButtons.contains(button)
        }
    }

    mutating func handle(_ event: HandsEvent) -> [Action] {
        switch event {
        case .arm:
            guard phase != .armed else { return [] }
            phase = .armed
            return []

        case .disarm:
            var actions = releaseAll()
            phase = .notArmed
            actions.append(.printLine(okLine(tick: 0)))
            return actions

        case .frame(let frame, let focusConfirmed, let cursorOverTarget, let nowNanos):
            return apply(
                frame,
                focusConfirmed: focusConfirmed,
                cursorOverTarget: cursorOverTarget,
                nowNanos: nowNanos)

        case .foreignInput:
            return takeover(.foreignInput)
        case .focusLost:
            return takeover(.focusLost)
        case .tapDisabled:
            return takeover(.tapDisabled)

        case .idleCheck(let nowNanos):
            return idleCheck(nowNanos: nowNanos)

        case .endOfInput(let reason):
            var actions = releaseAll()
            phase = .notArmed
            actions.append(.printLine(byeLine(reason: reason)))
            return actions
        }
    }

    private mutating func apply(
        _ frame: Frame,
        focusConfirmed: Bool,
        cursorOverTarget: Bool,
        nowNanos: UInt64
    ) -> [Action] {
        guard phase == .armed else {
            raiseGeneration(frame.generation)
            return [.printLine(refusedLine(tick: frame.tick, reason: "not_armed"))]
        }
        guard frame.generation >= highestGeneration else {
            return [.printLine(refusedLine(tick: frame.tick, reason: "stale"))]
        }
        raiseGeneration(frame.generation)

        guard focusConfirmed else { return takeover(.focusLost) }
        if frameRequestsButtonDown(frame) && !cursorOverTarget {
            return takeover(.focusLost)
        }

        let targetKeys = Set(
            keyBindings.filter { frame.keys & $0.bit != 0 }.map { $0.code })
        let targetButtons = Set(
            MouseButton.allCases.filter { frame.buttons & $0.mask != 0 })

        var actions: [Action] = []

        // 1. Releases: keys, then buttons.
        for binding in keyBindings where heldKeys.contains(binding.code) {
            guard !targetKeys.contains(binding.code) else { continue }
            actions.append(.postKey(code: binding.code, down: false))
            heldKeys.remove(binding.code)
        }
        for button in MouseButton.allCases where heldButtons.contains(button) {
            guard !targetButtons.contains(button) else { continue }
            actions.append(.postButton(button: button, down: false))
            heldButtons.remove(button)
        }

        // 2. One mouse move, typed by the buttons held after the releases.
        if frame.dx != 0 || frame.dy != 0 {
            let kind: MoveKind
            if heldButtons.contains(.left) {
                kind = .leftDragged
            } else if heldButtons.contains(.right) {
                kind = .rightDragged
            } else {
                kind = .moved
            }
            actions.append(.postMove(dx: frame.dx, dy: frame.dy, kind: kind))
        }

        // 3. Presses: keys, then buttons.
        for binding in keyBindings where targetKeys.contains(binding.code) {
            guard !heldKeys.contains(binding.code) else { continue }
            actions.append(.postKey(code: binding.code, down: true))
            heldKeys.insert(binding.code)
        }
        for button in MouseButton.allCases where targetButtons.contains(button) {
            guard !heldButtons.contains(button) else { continue }
            actions.append(.postButton(button: button, down: true))
            heldButtons.insert(button)
        }

        // 4. Slot tap: one press and one release.
        if let slot = frame.slot {
            let code = slotKeyCodes[slot - 1]
            actions.append(.postKey(code: code, down: true))
            actions.append(.postKey(code: code, down: false))
        }

        lastFrameNanos = nowNanos
        actions.append(.printLine(okLine(tick: frame.tick)))
        return actions
    }

    private mutating func raiseGeneration(_ generation: UInt64) {
        if generation > highestGeneration { highestGeneration = generation }
    }

    private mutating func idleCheck(nowNanos: UInt64) -> [Action] {
        guard phase == .armed, isHolding, let last = lastFrameNanos else { return [] }
        guard nowNanos >= last, nowNanos - last >= Tunables.idleTimeoutNanoseconds else { return [] }
        var actions = releaseAll()
        actions.append(.printLine(idleLine()))
        lastFrameNanos = nowNanos
        return actions
    }

    private mutating func takeover(_ reason: TakeoverReason) -> [Action] {
        guard phase == .armed else { return [] }
        var actions = releaseAll()
        phase = .notArmed
        actions.append(.printLine(takeoverLine(reason)))
        return actions
    }

    /// Idempotent: posts an up only for what is still held, then forgets it.
    private mutating func releaseAll() -> [Action] {
        var actions: [Action] = []
        for binding in keyBindings where heldKeys.contains(binding.code) {
            actions.append(.postKey(code: binding.code, down: false))
        }
        heldKeys.removeAll()
        for button in MouseButton.allCases where heldButtons.contains(button) {
            actions.append(.postButton(button: button, down: false))
        }
        heldButtons.removeAll()
        return actions
    }
}

// MARK: - Command line (pure)

enum Mode: Equatable {
    case selfTest
    case preflight
    case serve
    case releaseAll
}

struct Options: Equatable {
    let mode: Mode
    let pid: pid_t
    let activate: Bool
}

struct ArgumentError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

let usageText = """
usage:
  praxion-hands --self-test
  praxion-hands --preflight
  praxion-hands --serve --pid <pid> [--activate]
  praxion-hands --release-all --pid <pid>

modes (exactly one):
  --self-test    headless tests of the parser and the state machine; needs no permission
  --preflight    print the Quartz post and listen permission booleans; never prompts
  --serve        live mode: one frame line per client tick on stdin, replies on stdout
  --release-all  post key-up for every key and button-up for both buttons, then exit 0

serve and release-all options:
  --pid PID      the Minecraft (Java) process that owns the session
  --activate     ask macOS to bring the target forward before waiting for focus

exit codes: 0 ok, 2 refused (arguments, permissions, target)
"""

func parseArguments(_ args: [String]) -> Result<Options, ArgumentError> {
    var mode: Mode?
    var pid: pid_t?
    var activate = false
    var index = 0

    while index < args.count {
        let token = args[index]
        switch token {
        case "--self-test", "--preflight", "--serve", "--release-all":
            guard mode == nil else {
                return .failure(ArgumentError("more than one mode flag"))
            }
            switch token {
            case "--self-test": mode = .selfTest
            case "--preflight": mode = .preflight
            case "--serve": mode = .serve
            default: mode = .releaseAll
            }
            index += 1

        case "--activate":
            guard !activate else { return .failure(ArgumentError("--activate given twice")) }
            activate = true
            index += 1

        case "--pid":
            guard pid == nil else { return .failure(ArgumentError("--pid given twice")) }
            guard index + 1 < args.count else {
                return .failure(ArgumentError("--pid needs a value"))
            }
            let raw = args[index + 1]
            guard raw.count <= 10,
                  raw.allSatisfy({ $0.isASCII && $0.isWholeNumber }),
                  let value = Int32(raw), value > 0
            else {
                return .failure(ArgumentError("--pid must be a decimal process id above 0"))
            }
            pid = value
            index += 2

        default:
            let shown = token.count > 24 ? String(token.prefix(24)) + "..." : token
            return .failure(ArgumentError("unknown argument '\(shown)'"))
        }
    }

    guard let mode else { return .failure(ArgumentError("no mode given")) }

    switch mode {
    case .selfTest, .preflight:
        guard pid == nil else {
            return .failure(ArgumentError("--pid is only valid with --serve and --release-all"))
        }
        guard !activate else { return .failure(ArgumentError("--activate is only valid with --serve")) }
        return .success(Options(mode: mode, pid: 0, activate: false))

    case .serve:
        guard let pid else { return .failure(ArgumentError("--serve needs --pid")) }
        return .success(Options(mode: .serve, pid: pid, activate: activate))

    case .releaseAll:
        guard let pid else { return .failure(ArgumentError("--release-all needs --pid")) }
        guard !activate else { return .failure(ArgumentError("--activate is only valid with --serve")) }
        return .success(Options(mode: .releaseAll, pid: pid, activate: false))
    }
}

// MARK: - Permissions

struct Preflight {
    let postEvents: Bool
    let listenEvents: Bool

    var ok: Bool { postEvents && listenEvents }

    /// Never calls CGRequest*: this process must not change TCC state.
    static func read() -> Preflight {
        Preflight(
            postEvents: CGPreflightPostEventAccess(),
            listenEvents: CGPreflightListenEventAccess())
    }
}

func runPreflight() -> Int32 {
    let preflight = Preflight.read()
    emit("post_event_access=\(preflight.postEvents)")
    emit("listen_event_access=\(preflight.listenEvents)")
    if preflight.ok {
        emit("result=ok")
        return 0
    }
    emit("result=fail")
    return 2
}

// MARK: - Output

/// stdout is flushed after every line: the body reads replies line by line.
func emit(_ line: String) {
    print(line)
    fflush(stdout)
}

// MARK: - Self-test (pure, no OS)

func runSelfTest() -> Int32 {
    var checks = 0
    var failures = 0

    func check(_ name: String, _ condition: Bool) {
        checks += 1
        if !condition {
            failures += 1
            print("fail \(name)")
        }
    }

    func expect(_ name: String, _ got: [Action], _ want: [Action]) {
        let same = got == want
        check(name, same)
        if !same {
            print("detail \(name) got=\(got) want=\(want)")
        }
    }

    func key(_ code: Int, _ down: Bool) -> Action { .postKey(code: code, down: down) }
    func mouse(_ button: MouseButton, _ down: Bool) -> Action {
        .postButton(button: button, down: down)
    }
    func move(_ dx: Int, _ dy: Int, _ kind: MoveKind) -> Action {
        .postMove(dx: dx, dy: dy, kind: kind)
    }
    func text(_ line: String) -> Action { .printLine(line) }
    func ok(_ tick: UInt64) -> Action { .printLine(okLine(tick: tick)) }
    func refused(_ tick: UInt64, _ reason: String) -> Action {
        .printLine(refusedLine(tick: tick, reason: reason))
    }
    func takeover(_ reason: TakeoverReason) -> Action {
        .printLine(takeoverLine(reason))
    }
    func frame(
        _ tick: UInt64,
        _ generation: UInt64,
        keys: UInt8 = 0,
        dx: Int = 0,
        dy: Int = 0,
        buttons: UInt8 = 0,
        slot: Int? = nil
    ) -> Frame {
        Frame(
            tick: tick, generation: generation, keys: keys, dx: dx, dy: dy,
            buttons: buttons, slot: slot)
    }
    func applied(
        _ f: Frame,
        focus: Bool = true,
        cursor: Bool = true,
        now: UInt64 = 0
    ) -> HandsEvent {
        .frame(f, focusConfirmed: focus, cursorOverTarget: cursor, nowNanos: now)
    }
    func nothingHeld(_ name: String, _ machine: HandsMachine) {
        check("\(name)/no_keys_held", machine.heldKeys.isEmpty)
        check("\(name)/no_buttons_held", machine.heldButtons.isEmpty)
    }

    // Parsing: every line kind.
    func parsed(_ name: String, _ line: String, _ want: ProtocolLine) {
        let got = parseLine(line)
        check("parse/\(name)", got == want)
        if got != want {
            print("detail parse/\(name) got=\(got) want=\(want)")
        }
    }

    parsed("arm", "ARM", .arm)
    parsed("disarm", "DISARM", .disarm)
    parsed("frame_minimal", "F 0 0 0 0 0 0 -", .frame(frame(0, 0)))
    parsed("frame_full", "F 1 2 31 -600 600 3 9",
           .frame(frame(1, 2, keys: 31, dx: -600, dy: 600, buttons: 3, slot: 9)))
    parsed("frame_no_slot", "F 7 3 4 12 -12 2 -",
           .frame(frame(7, 3, keys: 4, dx: 12, dy: -12, buttons: 2)))
    parsed("frame_negative_zero", "F 1 0 0 -0 0 0 -", .frame(frame(1, 0)))
    parsed("frame_max_tick", "F 18446744073709551615 0 0 0 0 0 -",
           .frame(frame(UInt64.max, 0)))
    parsed("frame_slot_one", "F 4 4 0 0 0 0 1", .frame(frame(4, 4, slot: 1)))

    let malformedLines = [
        "",
        " ",
        "ARM ",
        "DISARM ",
        "arm",
        "Arm",
        "ARM 1",
        "DISARM now",
        "F",
        "F 1",
        "F 1 0 1 0 0 0",
        "F 1 0 1 0 0 0 - 2",
        "F 1 0 1  0 0 0 -",
        " F 1 0 1 0 0 0 -",
        "F 1 0 1 0 0 0 - ",
        "F a 0 1 0 0 0 -",
        "F 1 a 1 0 0 0 -",
        "F 1 0 a 0 0 0 -",
        "F 1 0 1 a 0 0 -",
        "F 1 0 1 0 a 0 -",
        "F 1 0 1 0 0 a -",
        "F 1 0 1 0 0 0 a",
        "F 1 0 -1 0 0 0 -",
        "F -1 0 1 0 0 0 -",
        "F 1 0 1 0 +1 0 -",
        "F 1 0 1 0 1.5 0 -",
        "F 1 0 1 601 0 0 -",
        "F 1 0 1 0 -601 0 -",
        "F 1 0 32 0 0 0 -",
        "F 1 0 1 0 0 4 -",
        "F 1 0 1 0 0 0 0",
        "F 1 0 1 0 0 0 10",
        "F 1 0 1 0 0 0 -1",
        "F 18446744073709551616 0 1 0 0 0 -",
        "F 1 0 1 0 0 0 - extra",
        "GO",
        "F\t1 0 1 0 0 0 -",
    ]
    for (index, line) in malformedLines.enumerated() {
        check("parse/malformed/\(index)", parseLine(line) == .malformed)
    }
    check("parse/slot_table", slotKeyCodes.count == 9)

    // ARM, DISARM, ARM again.
    do {
        var machine = HandsMachine()
        check("arm/starts_not_armed", !machine.isArmed)
        expect("arm/from_initial", machine.handle(.arm), [])
        check("arm/armed", machine.isArmed)
        expect("arm/ignored_while_armed", machine.handle(.arm), [])
        expect("disarm/releases_nothing", machine.handle(.disarm), [text("OK 0")])
        check("disarm/not_armed", !machine.isArmed)
        expect("disarm/idempotent", machine.handle(.disarm), [text("OK 0")])
        expect("arm/after_disarm", machine.handle(.arm), [])
        check("arm/armed_again", machine.isArmed)
    }

    // Frame before ARM.
    do {
        var machine = HandsMachine()
        expect("not_armed/refused",
               machine.handle(applied(frame(7, 0, keys: 1))), [refused(7, "not_armed")])
    }

    // Key diff.
    do {
        var machine = HandsMachine()
        expect("keys/session", machine.handle(.arm), [])
        expect("keys/press", machine.handle(applied(frame(1, 0, keys: 1))),
               [key(13, true), ok(1)])
        expect("keys/hold", machine.handle(applied(frame(2, 0, keys: 1))), [ok(2)])
        expect("keys/release", machine.handle(applied(frame(3, 0, keys: 0))),
               [key(13, false), ok(3)])
        expect("keys/several", machine.handle(applied(frame(4, 0, keys: 5))),
               [key(13, true), key(0, true), ok(4)])
        expect("keys/partial_release", machine.handle(applied(frame(5, 0, keys: 1))),
               [key(0, false), ok(5)])
        expect("keys/back_and_jump", machine.handle(applied(frame(6, 0, keys: 0b1_0010))),
               [key(13, false), key(1, true), key(49, true), ok(6)])
        expect("keys/all_up", machine.handle(applied(frame(7, 0, keys: 0))),
               [key(1, false), key(49, false), ok(7)])
        nothingHeld("keys/empty", machine)
    }

    // Button diff.
    do {
        var machine = HandsMachine()
        expect("buttons/session", machine.handle(.arm), [])
        expect("buttons/left_down", machine.handle(applied(frame(1, 0, buttons: 1))),
               [mouse(.left, true), ok(1)])
        expect("buttons/right_down", machine.handle(applied(frame(2, 0, buttons: 3))),
               [mouse(.right, true), ok(2)])
        expect("buttons/both_up", machine.handle(applied(frame(3, 0, buttons: 0))),
               [mouse(.left, false), mouse(.right, false), ok(3)])
        expect("buttons/both_down", machine.handle(applied(frame(4, 0, buttons: 3))),
               [mouse(.left, true), mouse(.right, true), ok(4)])
        expect("buttons/right_up", machine.handle(applied(frame(5, 0, buttons: 1))),
               [mouse(.right, false), ok(5)])
        check("buttons/left_still_held", machine.heldButtons == [.left])
        expect("buttons/last_up", machine.handle(applied(frame(6, 0, buttons: 0))),
               [mouse(.left, false), ok(6)])
        nothingHeld("buttons/empty", machine)
    }

    // Order of posts inside one frame.
    do {
        var machine = HandsMachine()
        expect("order/session", machine.handle(.arm), [])
        expect("order/setup", machine.handle(applied(frame(1, 0, keys: 1, buttons: 1))),
               [key(13, true), mouse(.left, true), ok(1)])
        expect(
            "order/full",
            machine.handle(applied(frame(2, 0, keys: 2, dx: 10, dy: -5, buttons: 2, slot: 3))),
            [
                key(13, false),
                mouse(.left, false),
                move(10, -5, .moved),
                key(1, true),
                mouse(.right, true),
                key(20, true),
                key(20, false),
                ok(2),
            ])
    }

    // Drag event type selection.
    do {
        var machine = HandsMachine()
        expect("drag/session", machine.handle(.arm), [])
        expect("drag/left_setup", machine.handle(applied(frame(1, 0, buttons: 1))),
               [mouse(.left, true), ok(1)])
        expect("drag/left", machine.handle(applied(frame(2, 0, dx: -7, dy: 3, buttons: 1))),
               [move(-7, 3, .leftDragged), ok(2)])
        expect("drag/left_up", machine.handle(applied(frame(3, 0))),
               [mouse(.left, false), ok(3)])
        expect("drag/right_setup", machine.handle(applied(frame(4, 0, buttons: 2))),
               [mouse(.right, true), ok(4)])
        expect("drag/right", machine.handle(applied(frame(5, 0, dx: 1, dy: -1, buttons: 2))),
               [move(1, -1, .rightDragged), ok(5)])
        expect("drag/both_setup", machine.handle(applied(frame(6, 0, buttons: 3))),
               [mouse(.left, true), ok(6)])
        expect("drag/both_prefers_left",
               machine.handle(applied(frame(7, 0, dx: 2, dy: 2, buttons: 3))),
               [move(2, 2, .leftDragged), ok(7)])
        expect("drag/none_setup", machine.handle(applied(frame(8, 0))),
               [mouse(.left, false), mouse(.right, false), ok(8)])
        expect("drag/none", machine.handle(applied(frame(9, 0, dx: 0, dy: -4))),
               [move(0, -4, .moved), ok(9)])
        expect("drag/zero_delta_skipped", machine.handle(applied(frame(10, 0))), [ok(10)])
    }

    // Slot taps: 1...9 in slot order.
    do {
        var machine = HandsMachine()
        expect("slot/session", machine.handle(.arm), [])
        let codes = [18, 19, 20, 21, 23, 22, 26, 28, 25]
        check("slot/codes_match", slotKeyCodes == codes)
        for slot in 1...9 {
            let tick = UInt64(slot)
            expect("slot/\(slot)", machine.handle(applied(frame(tick, 0, slot: slot))),
                   [key(codes[slot - 1], true), key(codes[slot - 1], false), ok(tick)])
        }
        nothingHeld("slot/empty", machine)
    }

    // Stale generation.
    do {
        var machine = HandsMachine()
        expect("generation/session", machine.handle(.arm), [])
        expect("generation/first", machine.handle(applied(frame(1, 5, keys: 1))),
               [key(13, true), ok(1)])
        expect("generation/stale", machine.handle(applied(frame(2, 4, keys: 0))),
               [refused(2, "stale")])
        expect("generation/still_held", machine.handle(applied(frame(3, 5, keys: 1))), [ok(3)])
        expect("generation/bumped", machine.handle(applied(frame(4, 6, keys: 0))),
               [key(13, false), ok(4)])
        expect("generation/equal_applies", machine.handle(applied(frame(5, 6))), [ok(5)])
        nothingHeld("generation/empty", machine)
    }

    // The generation watermark also moves while the helper is not armed.
    do {
        var machine = HandsMachine()
        expect("watermark/not_armed", machine.handle(applied(frame(1, 9))),
               [refused(1, "not_armed")])
        expect("watermark/arm", machine.handle(.arm), [])
        expect("watermark/stale_after_arm", machine.handle(applied(frame(2, 4))),
               [refused(2, "stale")])
        expect("watermark/current", machine.handle(applied(frame(3, 9, keys: 1))),
               [key(13, true), ok(3)])
        expect("watermark/newer", machine.handle(applied(frame(4, 10, keys: 0))),
               [key(13, false), ok(4)])
    }

    // Takeover on foreign input, then ARM again.
    do {
        var machine = HandsMachine()
        expect("foreign/session", machine.handle(.arm), [])
        expect("foreign/setup",
               machine.handle(applied(frame(1, 0, keys: 1, buttons: 1))),
               [key(13, true), mouse(.left, true), ok(1)])
        expect("foreign/takeover", machine.handle(.foreignInput),
               [key(13, false), mouse(.left, false), takeover(.foreignInput)])
        nothingHeld("foreign", machine)
        check("foreign/not_armed", !machine.isArmed)
        expect("foreign/frame_refused", machine.handle(applied(frame(2, 0))),
               [refused(2, "not_armed")])
        expect("foreign/second_signal_ignored", machine.handle(.foreignInput), [])
        expect("foreign/tap_ignored", machine.handle(.tapDisabled), [])
        expect("foreign/focus_ignored", machine.handle(.focusLost), [])
        expect("foreign/rearm", machine.handle(.arm), [])
        expect("foreign/frame_after_rearm", machine.handle(applied(frame(3, 0, keys: 1))),
               [key(13, true), ok(3)])
    }

    // DISARM releases what is held, before answering.
    do {
        var machine = HandsMachine()
        expect("disarm_held/session", machine.handle(.arm), [])
        expect("disarm_held/setup", machine.handle(applied(frame(1, 0, keys: 16, buttons: 1))),
               [key(49, true), mouse(.left, true), ok(1)])
        expect("disarm_held/release", machine.handle(.disarm),
               [key(49, false), mouse(.left, false), text("OK 0")])
        nothingHeld("disarm_held", machine)
        check("disarm_held/not_armed", !machine.isArmed)
    }

    // Foreign input while armed and holding nothing still ends the armed state.
    do {
        var machine = HandsMachine()
        expect("foreign_empty/session", machine.handle(.arm), [])
        expect("foreign_empty/takeover", machine.handle(.foreignInput),
               [takeover(.foreignInput)])
        nothingHeld("foreign_empty", machine)
        check("foreign_empty/not_armed", !machine.isArmed)
    }

    // Takeover on focus loss inside a frame.
    do {
        var machine = HandsMachine()
        expect("focus/session", machine.handle(.arm), [])
        expect("focus/setup",
               machine.handle(applied(frame(1, 0, keys: 1, buttons: 3))),
               [key(13, true), mouse(.left, true), mouse(.right, true), ok(1)])
        expect("focus/frame", machine.handle(applied(frame(2, 0, keys: 1, buttons: 3), focus: false)),
               [key(13, false), mouse(.left, false), mouse(.right, false), takeover(.focusLost)])
        nothingHeld("focus", machine)
        check("focus/not_armed", !machine.isArmed)
    }

    // Takeover from the activation observer.
    do {
        var machine = HandsMachine()
        expect("focus_event/session", machine.handle(.arm), [])
        expect("focus_event/setup", machine.handle(applied(frame(1, 0, keys: 16))),
               [key(49, true), ok(1)])
        expect("focus_event/takeover", machine.handle(.focusLost),
               [key(49, false), takeover(.focusLost)])
        nothingHeld("focus_event", machine)
    }

    // Cursor check: needed only for a new button-down.
    do {
        var machine = HandsMachine()
        check("cursor/not_armed_reports", machine.frameRequestsButtonDown(frame(1, 0, buttons: 1)))
        check("cursor/no_button", !machine.frameRequestsButtonDown(frame(1, 0, keys: 31)))
        expect("cursor/session", machine.handle(.arm), [])
        expect("cursor/no_button_down", machine.handle(applied(frame(1, 0, keys: 1), cursor: false)),
               [key(13, true), ok(1)])
        expect("cursor/button_down_refused",
               machine.handle(applied(frame(2, 0, keys: 1, buttons: 1), cursor: false)),
               [key(13, false), takeover(.focusLost)])
        nothingHeld("cursor", machine)
    }
    do {
        var machine = HandsMachine()
        expect("cursor_held/session", machine.handle(.arm), [])
        expect("cursor_held/setup", machine.handle(applied(frame(1, 0, buttons: 1))),
               [mouse(.left, true), ok(1)])
        check("cursor_held/not_requested", !machine.frameRequestsButtonDown(frame(2, 0, buttons: 1)))
        check("cursor_held/right_new", machine.frameRequestsButtonDown(frame(2, 0, buttons: 3)))
        expect("cursor_held/hold_ok", machine.handle(applied(frame(2, 0, buttons: 1), cursor: false)),
               [ok(2)])
        expect("cursor_held/up_ok", machine.handle(applied(frame(3, 0, buttons: 0), cursor: false)),
               [mouse(.left, false), ok(3)])
    }

    // Tap disabled.
    do {
        var machine = HandsMachine()
        expect("tap_disabled/session", machine.handle(.arm), [])
        expect("tap_disabled/setup",
               machine.handle(applied(frame(1, 0, keys: 1, buttons: 2))),
               [key(13, true), mouse(.right, true), ok(1)])
        expect("tap_disabled/takeover", machine.handle(.tapDisabled),
               [key(13, false), mouse(.right, false), takeover(.tapDisabled)])
        nothingHeld("tap_disabled", machine)
        check("tap_disabled/not_armed", !machine.isArmed)
    }

    // IDLE: 250 ms without a frame while something is held.
    do {
        var machine = HandsMachine()
        expect("idle/session", machine.handle(.arm), [])
        expect("idle/setup", machine.handle(applied(frame(1, 0, keys: 1), now: 1_000_000_000)),
               [key(13, true), ok(1)])
        expect("idle/before_deadline", machine.handle(.idleCheck(nowNanos: 1_249_999_999)), [])
        expect("idle/at_deadline", machine.handle(.idleCheck(nowNanos: 1_250_000_000)),
               [key(13, false), text("IDLE")])
        check("idle/still_armed", machine.isArmed)
        nothingHeld("idle", machine)
        expect("idle/no_repeat", machine.handle(.idleCheck(nowNanos: 9_000_000_000)), [])
        expect("idle/frame_after_idle",
               machine.handle(applied(frame(2, 0, keys: 1), now: 9_000_000_000)),
               [key(13, true), ok(2)])
    }
    do {
        var machine = HandsMachine()
        expect("idle_empty/session", machine.handle(.arm), [])
        expect("idle_empty/setup", machine.handle(applied(frame(1, 0), now: 0)), [ok(1)])
        expect("idle_empty/nothing_held", machine.handle(.idleCheck(nowNanos: 5_000_000_000)), [])
    }
    do {
        var machine = HandsMachine()
        expect("idle_not_armed/session", machine.handle(.idleCheck(nowNanos: 5_000_000_000)), [])
    }

    // End of input and signals.
    do {
        var machine = HandsMachine()
        expect("eof/session", machine.handle(.arm), [])
        expect("eof/setup",
               machine.handle(applied(frame(1, 0, keys: 3, buttons: 3))),
               [key(13, true), key(1, true), mouse(.left, true), mouse(.right, true), ok(1)])
        expect("eof/release", machine.handle(.endOfInput(reason: "eof")),
               [
                   key(13, false), key(1, false), mouse(.left, false), mouse(.right, false),
                   text("BYE eof"),
               ])
        nothingHeld("eof", machine)
        check("eof/not_armed", !machine.isArmed)
        expect("eof/after", machine.handle(applied(frame(2, 0))), [refused(2, "not_armed")])
        expect("eof/idempotent", machine.handle(.endOfInput(reason: "eof")), [text("BYE eof")])
    }
    do {
        var machine = HandsMachine()
        expect("signal/session", machine.handle(.arm), [])
        expect("signal/setup", machine.handle(applied(frame(1, 0, keys: 16))), [key(49, true), ok(1)])
        expect("signal/release", machine.handle(.endOfInput(reason: "sigterm")),
               [key(49, false), text("BYE sigterm")])
        nothingHeld("signal", machine)
    }
    do {
        var machine = HandsMachine()
        expect("signal_idle/session", machine.handle(.endOfInput(reason: "sigint")),
               [text("BYE sigint")])
        nothingHeld("signal_idle", machine)
    }

    // Release-all after a takeover is idempotent.
    do {
        var machine = HandsMachine()
        expect("release_all/session", machine.handle(.arm), [])
        expect("release_all/setup", machine.handle(applied(frame(1, 0, keys: 1, buttons: 3))),
               [key(13, true), mouse(.left, true), mouse(.right, true), ok(1)])
        expect("release_all/takeover", machine.handle(.foreignInput),
               [key(13, false), mouse(.left, false), mouse(.right, false), takeover(.foreignInput)])
        expect("release_all/disarm", machine.handle(.disarm), [text("OK 0")])
        expect("release_all/eof", machine.handle(.endOfInput(reason: "eof")), [text("BYE eof")])
    }

    // Argument parser.
    func argument(_ name: String, _ args: [String], success: Mode?) {
        switch (parseArguments(args), success) {
        case (.success(let options), .some(let want)):
            check("args/\(name)", options.mode == want)
        case (.failure, .none):
            check("args/\(name)", true)
        default:
            check("args/\(name)", false)
        }
    }

    argument("self_test", ["--self-test"], success: .selfTest)
    argument("preflight", ["--preflight"], success: .preflight)
    argument("serve", ["--serve", "--pid", "4242"], success: .serve)
    argument("serve_activate", ["--serve", "--pid", "4242", "--activate"], success: .serve)
    argument("release_all", ["--release-all", "--pid", "4242"], success: .releaseAll)
    argument("no_mode", [], success: nil)
    argument("two_modes", ["--serve", "--preflight"], success: nil)
    argument("serve_without_pid", ["--serve"], success: nil)
    argument("release_all_without_pid", ["--release-all"], success: nil)
    argument("activate_without_serve", ["--release-all", "--pid", "4242", "--activate"],
             success: nil)
    argument("activate_with_preflight", ["--preflight", "--activate"], success: nil)
    argument("pid_zero", ["--serve", "--pid", "0"], success: nil)
    argument("pid_negative", ["--serve", "--pid", "-3"], success: nil)
    argument("pid_not_decimal", ["--serve", "--pid", "12a"], success: nil)
    argument("pid_overflow", ["--serve", "--pid", "2147483648"], success: nil)
    argument("pid_twice", ["--serve", "--pid", "4", "--pid", "5"], success: nil)
    argument("pid_missing_value", ["--serve", "--pid"], success: nil)
    argument("activate_twice", ["--serve", "--pid", "4", "--activate", "--activate"],
             success: nil)
    argument("unknown_flag", ["--serve", "--pid", "4", "--keycode", "13"], success: nil)
    argument("pid_with_self_test", ["--self-test", "--pid", "4"], success: nil)
    argument("empty_token", [""], success: nil)

    if let options = try? parseArguments(["--serve", "--pid", "4242", "--activate"]).get() {
        check("args/serve/pid", options.pid == 4242)
        check("args/serve/activate", options.activate)
    } else {
        check("args/serve/parses", false)
    }
    if let options = try? parseArguments(["--release-all", "--pid", "7"]).get() {
        check("args/release_all/pid", options.pid == 7)
        check("args/release_all/no_activate", !options.activate)
    } else {
        check("args/release_all/parses", false)
    }
    if let options = try? parseArguments(["--preflight"]).get() {
        check("args/preflight/pid_zero", options.pid == 0)
    } else {
        check("args/preflight/parses", false)
    }

    print("self_test_checks=\(checks) self_test_failures=\(failures)")
    if failures == 0 {
        print("result=ok")
        return 0
    }
    print("result=failed")
    return 1
}

// MARK: - Event posting (the only place that touches the OS input API)

final class EventPoster {
    private let pid: pid_t
    private let source: CGEventSource

    init?(pid: pid_t) {
        guard let source = CGEventSource(stateID: .hidSystemState) else { return nil }
        self.pid = pid
        self.source = source
    }

    /// The pointer is never moved by the helper; global events land where it is.
    static func cursorPosition() -> CGPoint? {
        CGEvent(source: nil)?.location
    }

    /// Keyboard goes to the target PID: the measured path that reaches the client.
    func key(code: Int, down: Bool) {
        guard let event = CGEvent(
            keyboardEventSource: source, virtualKey: CGKeyCode(code), keyDown: down)
        else { return }
        tag(event)
        event.postToPid(pid)
    }

    /// Mouse buttons are posted globally at the session tap; a PID-targeted
    /// button does not reach the client.
    func button(_ button: MouseButton, down: Bool) {
        guard let point = EventPoster.cursorPosition() else { return }
        let type: CGEventType
        let cgButton: CGMouseButton
        if button == .left {
            type = down ? .leftMouseDown : .leftMouseUp
            cgButton = .left
        } else {
            type = down ? .rightMouseDown : .rightMouseUp
            cgButton = .right
        }
        guard let event = CGEvent(
            mouseEventSource: source, mouseType: type, mouseCursorPosition: point,
            mouseButton: cgButton)
        else { return }
        event.setIntegerValueField(.mouseEventClickState, value: Tunables.clickState)
        tag(event)
        event.post(tap: .cgSessionEventTap)
    }

    /// Camera movement is a mouse event carrying deltas, at the cursor position.
    func move(dx: Int, dy: Int, kind: MoveKind) {
        guard let point = EventPoster.cursorPosition() else { return }
        let type: CGEventType
        let cgButton: CGMouseButton
        switch kind {
        case .moved:
            type = .mouseMoved
            cgButton = .left
        case .leftDragged:
            type = .leftMouseDragged
            cgButton = .left
        case .rightDragged:
            type = .rightMouseDragged
            cgButton = .right
        }
        guard let event = CGEvent(
            mouseEventSource: source, mouseType: type, mouseCursorPosition: point,
            mouseButton: cgButton)
        else { return }
        event.setIntegerValueField(.mouseEventDeltaX, value: Int64(dx))
        event.setIntegerValueField(.mouseEventDeltaY, value: Int64(dy))
        tag(event)
        event.post(tap: .cgSessionEventTap)
    }

    private func tag(_ event: CGEvent) {
        event.setIntegerValueField(.eventSourceUserData, value: Tunables.eventTag)
        event.flags = CGEventFlags()
    }
}

// MARK: - Release-all (used by the body after the helper died)

func runReleaseAll(pid: pid_t) -> Int32 {
    guard let poster = EventPoster(pid: pid) else { return 0 }
    for binding in keyBindings {
        poster.key(code: binding.code, down: false)
    }
    for code in slotKeyCodes {
        poster.key(code: code, down: false)
    }
    for button in MouseButton.allCases {
        poster.button(button, down: false)
    }
    return 0
}

// MARK: - Live serve mode

final class HandsRunner {
    private let options: Options
    private var machine = HandsMachine()
    private var poster: EventPoster?

    private var eventTap: CFMachPort?
    private var tapSource: CFRunLoopSource?
    private var timer: Timer?
    private var activationObserver: NSObjectProtocol?
    private var signalSources: [DispatchSourceSignal] = []

    private let pendingLock = NSLock()
    private var pendingLines: [String] = []
    private var pendingEnd: String?

    private var pendingTakeover: TakeoverReason?
    private var pendingExit: String?
    private var finished = false

    init(options: Options) {
        self.options = options
    }

    // MARK: Entry

    func run() -> Int32 {
        // A closed or dead stdout must not kill the process with SIGPIPE: the
        // release path has to run. The write then fails silently and the stdin
        // EOF ends the session properly.
        signal(SIGPIPE, SIG_IGN)

        let preflight = Preflight.read()
        guard preflight.ok else { return refuse("missing_permissions") }
        guard options.pid != getpid() else { return refuse("target_is_self") }
        guard let executablePath = targetExecutablePath(options.pid) else {
            return refuse("target_unavailable")
        }
        guard isMinecraftProcess(executablePath: executablePath) else {
            return refuse("target_not_minecraft")
        }

        installSignalHandlers()

        if options.activate, let app = NSRunningApplication(processIdentifier: options.pid) {
            _ = app.activate(options: [.activateAllWindows])
        }
        guard waitForFrontmost() else {
            if let reason = pendingExit {
                pendingExit = nil
                emit(byeLine(reason: reason))
                return 0
            }
            return refuse("target_not_frontmost")
        }

        guard let poster = EventPoster(pid: options.pid) else {
            return refuse("event_source_unavailable")
        }
        self.poster = poster
        guard createListenTap() else { return refuse("event_tap_unavailable") }
        addActivationObserver()

        startStdinReader()
        emit(readyLine())

        let timer = Timer(
            timeInterval: Double(Tunables.tickNanoseconds) / 1_000_000_000, repeats: true
        ) { [weak self] _ in
            self?.tick()
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer

        // No arm deadline: the body decides when to ARM.
        CFRunLoopRun()
        return 0
    }

    // MARK: Preconditions

    private func targetExecutablePath(_ pid: pid_t) -> String? {
        var buffer = [CChar](repeating: 0, count: 4096)
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        let bytes = buffer.prefix(Int(length)).map { UInt8(bitPattern: $0) }
        return String(decoding: bytes, as: UTF8.self)
    }

    /// Coarse executable check, the same one the spike used. The caller must
    /// supply the client's PID.
    private func isMinecraftProcess(executablePath: String) -> Bool {
        let lowercased = executablePath.lowercased()
        let name = (lowercased as NSString).lastPathComponent
        return name == "java" || lowercased.contains("/minecraft")
            || lowercased.contains("/runtime/java")
    }

    private func isTargetForeground() -> Bool {
        guard let front = NSWorkspace.shared.frontmostApplication else { return false }
        return front.processIdentifier == options.pid && front.isActive
    }

    private func waitForFrontmost() -> Bool {
        let deadline = monotonicNanos() + Tunables.focusWaitNanoseconds
        while monotonicNanos() < deadline {
            if pendingExit != nil { return false }
            if isTargetForeground() { return true }
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        return isTargetForeground()
    }

    /// A global click lands on the topmost window under the pointer, so that
    /// window must belong to the target. The window list is front to back; the
    /// first normal-layer window containing the point decides.
    private func cursorIsOverTarget() -> Bool {
        guard let point = EventPoster.cursorPosition(),
              let windows = CGWindowListCopyWindowInfo(
                  [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
                  as? [[String: Any]]
        else { return false }
        for window in windows {
            guard (window[kCGWindowLayer as String] as? Int) == 0,
                  let bounds = window[kCGWindowBounds as String] as? NSDictionary,
                  let rect = CGRect(dictionaryRepresentation: bounds),
                  rect.contains(point)
            else { continue }
            return (window[kCGWindowOwnerPID as String] as? Int32) == options.pid
        }
        return false
    }

    // MARK: Tap

    private func createListenTap() -> Bool {
        let types: [CGEventType] = [
            .keyDown, .flagsChanged,
            .leftMouseDown, .rightMouseDown, .otherMouseDown,
            .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged,
            .scrollWheel,
        ]
        var mask: CGEventMask = 0
        for type in types {
            mask |= CGEventMask(1) << CGEventMask(type.rawValue)
        }

        guard
            let tap = CGEvent.tapCreate(
                tap: .cgSessionEventTap,
                place: .tailAppendEventTap,
                options: .listenOnly,
                eventsOfInterest: mask,
                callback: handsTapCallback,
                userInfo: Unmanaged.passUnretained(self).toOpaque())
        else { return false }
        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0) else {
            CFMachPortInvalidate(tap)
            return false
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        eventTap = tap
        tapSource = source
        return true
    }

    /// C callback on the main run loop: records intent only, never posts.
    func handleTap(type: CGEventType, event: CGEvent) {
        precondition(Thread.isMainThread, "tap callback must stay on the main run loop")

        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            requestTakeover(.tapDisabled)
            return
        }
        if event.getIntegerValueField(.eventSourceUserData) != Tunables.eventTag {
            requestTakeover(.foreignInput)
        }
    }

    private func addActivationObserver() {
        activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            guard let self, self.machine.isArmed, !self.isTargetForeground() else { return }
            self.requestTakeover(.focusLost)
        }
    }

    private func requestTakeover(_ reason: TakeoverReason) {
        if pendingTakeover == nil { pendingTakeover = reason }
    }

    // MARK: Signals and stdin

    private func installSignalHandlers() {
        let handled: [(Int32, String)] = [(SIGINT, "sigint"), (SIGTERM, "sigterm")]
        for (signalNumber, name) in handled {
            signal(signalNumber, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: signalNumber, queue: .main)
            source.setEventHandler { [weak self] in
                guard let self, !self.finished else { return }
                if self.pendingExit == nil { self.pendingExit = name }
                CFRunLoopWakeUp(CFRunLoopGetMain())
            }
            source.resume()
            signalSources.append(source)
        }
    }

    /// Own thread: reads bytes, splits complete lines, hands them to the main
    /// run loop. It never touches the machine or the OS input API.
    private func startStdinReader() {
        let thread = Thread { [weak self] in
            var buffer: [UInt8] = []
            var chunk = [UInt8](repeating: 0, count: 4096)
            while true {
                let count = chunk.withUnsafeMutableBytes { raw -> Int in
                    read(STDIN_FILENO, raw.baseAddress, raw.count)
                }
                if count > 0 {
                    buffer.append(contentsOf: chunk[0..<count])
                    var lines: [String] = []
                    while let newline = buffer.firstIndex(of: 0x0A) {
                        lines.append(String(decoding: buffer[0..<newline], as: UTF8.self))
                        buffer.removeSubrange(0...newline)
                    }
                    if !lines.isEmpty { self?.enqueue(lines: lines) }
                } else if count == 0 {
                    self?.enqueueEnd("eof")
                    return
                } else if errno != EINTR {
                    self?.enqueueEnd("eof")
                    return
                }
            }
        }
        thread.name = "praxion-hands-stdin"
        thread.start()
    }

    private func enqueue(lines: [String]) {
        pendingLock.lock()
        pendingLines.append(contentsOf: lines)
        pendingLock.unlock()
        DispatchQueue.main.async { [weak self] in self?.tick() }
    }

    private func enqueueEnd(_ reason: String) {
        pendingLock.lock()
        if pendingEnd == nil { pendingEnd = reason }
        pendingLock.unlock()
        DispatchQueue.main.async { [weak self] in self?.tick() }
    }

    // MARK: Main-thread tick

    private func tick() {
        guard !finished else { return }
        drainPendingInput()
        guard !finished else { return }

        if let reason = pendingExit {
            pendingExit = nil
            finish(withReason: reason)
            return
        }
        if let reason = pendingTakeover {
            pendingTakeover = nil
            if machine.isArmed {
                perform(machine.handle(event(for: reason)))
                return
            }
        }

        guard machine.isArmed else { return }
        if let eventTap, !CGEvent.tapIsEnabled(tap: eventTap) {
            perform(machine.handle(.tapDisabled))
            return
        }
        if !isTargetForeground() {
            perform(machine.handle(.focusLost))
            return
        }
        perform(machine.handle(.idleCheck(nowNanos: monotonicNanos())))
    }

    private func drainPendingInput() {
        pendingLock.lock()
        let lines = pendingLines
        pendingLines.removeAll()
        let end = pendingEnd
        pendingEnd = nil
        pendingLock.unlock()

        for line in lines {
            guard !finished else { return }
            handle(line: line)
        }
        if let end {
            finish(withReason: end)
        }
    }

    private func handle(line: String) {
        switch parseLine(line) {
        case .malformed:
            emit(refusedLine(tick: 0, reason: "malformed"))
        case .arm:
            perform(machine.handle(.arm))
        case .disarm:
            perform(machine.handle(.disarm))
        case .frame(let frame):
            apply(frame)
        }
    }

    private func apply(_ frame: Frame) {
        // A disabled tap cannot witness foreign input, so the frame must not be
        // applied on top of it. This closes the gap between the ARM line and
        // the next tick.
        if machine.isArmed, let eventTap, !CGEvent.tapIsEnabled(tap: eventTap) {
            perform(machine.handle(.tapDisabled))
        }

        var focusConfirmed = true
        var cursorOverTarget = true
        if machine.isArmed {
            focusConfirmed = isTargetForeground()
            if focusConfirmed && machine.frameRequestsButtonDown(frame) {
                cursorOverTarget = cursorIsOverTarget()
            }
        }
        perform(
            machine.handle(
                .frame(
                    frame,
                    focusConfirmed: focusConfirmed,
                    cursorOverTarget: cursorOverTarget,
                    nowNanos: monotonicNanos())))
    }

    private func event(for reason: TakeoverReason) -> HandsEvent {
        switch reason {
        case .foreignInput: return .foreignInput
        case .focusLost: return .focusLost
        case .tapDisabled: return .tapDisabled
        }
    }

    private func perform(_ actions: [Action]) {
        for action in actions {
            switch action {
            case .postKey(let code, let down):
                poster?.key(code: code, down: down)
            case .postButton(let button, let down):
                poster?.button(button, down: down)
            case .postMove(let dx, let dy, let kind):
                poster?.move(dx: dx, dy: dy, kind: kind)
            case .printLine(let line):
                emit(line)
            }
        }
    }

    private func finish(withReason reason: String) {
        guard !finished else { return }
        finished = true
        perform(machine.handle(.endOfInput(reason: reason)))
        shutdown()
    }

    private func shutdown() {
        timer?.invalidate()
        timer = nil
        if let tap = eventTap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let source = tapSource {
                CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
            }
            CFMachPortInvalidate(tap)
            eventTap = nil
            tapSource = nil
        }
        if let observer = activationObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(observer)
            activationObserver = nil
        }
        for source in signalSources {
            source.cancel()
        }
        signalSources.removeAll()
        CFRunLoopStop(CFRunLoopGetMain())
    }

    private func refuse(_ reason: String) -> Int32 {
        emit(refusedLine(tick: 0, reason: reason))
        return 2
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
    case .selfTest:
        exit(runSelfTest())
    case .preflight:
        exit(runPreflight())
    case .serve:
        exit(HandsRunner(options: options).run())
    case .releaseAll:
        exit(runReleaseAll(pid: options.pid))
    }
}
