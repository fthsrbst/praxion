// Mouse delivery probe for the macOS hands spike.
//
// Question: which Quartz post path makes the vanilla client accept a mouse
// button and a camera move? The PID-targeted click did not break a block
// (logs/mac-dashboard-click-main-window/result.txt). This probe posts one
// bounded click or one bounded camera move through a chosen path:
//
//   hid      CGEvent.post(tap: .cghidEventTap)       global, enters at the HID level
//   session  CGEvent.post(tap: .cgSessionEventTap)   global, enters at the session level
//   pid      CGEvent.postToPid                       the path that failed for the click
//
// It posts only while the target PID is the frontmost application, tags every
// event (`eventSourceUserData`), always posts the button-up, and ends in well
// under a second. It has no listen tap: takeover during the probe is not
// tested here (main.swift owns that). Use only in a disposable test world.
//
//   xcrun swiftc -warnings-as-errors spikes/macos-hands/mouse_probe.swift -o logs/mac-hands/mouse-probe
//   logs/mac-hands/mouse-probe --pid PID --tap hid --click left
//   logs/mac-hands/mouse-probe --pid PID --tap hid --look 300 0
//   logs/mac-hands/mouse-probe --pid PID --tap session --click left --hold-ms 1500
//
// `--activate` asks macOS to bring the target forward first (the fixture uses it for the
// client it launched itself); without it the probe waits 15 s for a person to do so.
//
// Exit codes: 0 posted, 2 refused (arguments, permission, focus), 3 aborted mid-way.

import AppKit
import CoreGraphics
import Foundation

private let eventTag: Int64 = 0x5052_4148  // "PRAH", same tag as main.swift
private let focusWaitSeconds = 15.0
private let lookSteps = 30
private let lookStepSeconds = 0.01

enum Path: String { case hid, session, pid }
enum Action { case click(CGMouseButton), look(dx: Int, dy: Int) }

func refuse(_ reason: String) -> Never {
    print("result=refused")
    print("reason=\(reason)")
    exit(2)
}

// MARK: Arguments

var pid: pid_t?
var path: Path?
var action: Action?
var activate = false
var holdMs = 120
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let flag = args.removeFirst()
    switch flag {
    case "--pid":
        guard let raw = args.first, let value = Int32(raw), value > 1 else { refuse("bad_pid") }
        args.removeFirst()
        pid = value
    case "--tap":
        guard let raw = args.first, let value = Path(rawValue: raw) else { refuse("bad_tap") }
        args.removeFirst()
        path = value
    case "--activate":
        activate = true
    case "--click":
        guard let raw = args.first, raw == "left" || raw == "right" else { refuse("bad_click") }
        args.removeFirst()
        action = .click(raw == "left" ? .left : .right)
    case "--hold-ms":
        guard let raw = args.first, let value = Int(raw), (20...3000).contains(value) else { refuse("bad_hold_ms") }
        args.removeFirst()
        holdMs = value
    case "--look":
        guard args.count >= 2, let dx = Int(args[0]), let dy = Int(args[1]),
              abs(dx) <= 2000, abs(dy) <= 2000
        else { refuse("bad_look") }
        args.removeFirst(2)
        action = .look(dx: dx, dy: dy)
    default:
        refuse("unknown_argument")
    }
}
guard let pid, let path, let action else { refuse("need_pid_tap_and_one_action") }

// MARK: Preconditions

guard CGPreflightPostEventAccess() else { refuse("no_post_event_access") }
guard pid != getpid() else { refuse("target_is_self") }

func targetIsFront() -> Bool {
    guard let front = NSWorkspace.shared.frontmostApplication else { return false }
    return front.processIdentifier == pid && front.isActive
}

// Fixture only: bring the disposable test client forward instead of waiting for a person.
if activate, let app = NSRunningApplication(processIdentifier: pid) {
    print("activate_requested=\(app.activate(options: [.activateAllWindows]))")
}
let focusDeadline = Date().addingTimeInterval(focusWaitSeconds)
while !targetIsFront() && Date() < focusDeadline {
    RunLoop.main.run(until: Date().addingTimeInterval(0.05))
}
guard targetIsFront() else { refuse("target_not_frontmost") }
guard !CGEventSource.buttonState(.hidSystemState, button: .left),
      !CGEventSource.buttonState(.hidSystemState, button: .right)
else { refuse("a_mouse_button_is_already_down") }
guard let source = CGEventSource(stateID: .hidSystemState) else { refuse("no_event_source") }

/// The cursor's current position: the probe never moves the pointer itself.
func cursor() -> CGPoint? { CGEvent(source: nil)?.location }

/// A global click lands on the topmost window under the pointer, so that
/// window must belong to the target. The window list is front to back; the
/// first normal-layer window containing the point decides.
func cursorIsOverTarget() -> Bool {
    guard let point = cursor(),
          let windows = CGWindowListCopyWindowInfo(
              [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]]
    else { return false }
    for window in windows {
        guard (window[kCGWindowLayer as String] as? Int) == 0,
              let bounds = window[kCGWindowBounds as String] as? NSDictionary,
              let rect = CGRect(dictionaryRepresentation: bounds),
              rect.contains(point)
        else { continue }
        return (window[kCGWindowOwnerPID as String] as? Int32) == pid
    }
    return false
}
guard cursorIsOverTarget() else { refuse("cursor_outside_target_window") }

// A termination signal during the 120 ms hold must not skip the button-up.
signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)

func post(_ event: CGEvent) {
    event.setIntegerValueField(.eventSourceUserData, value: eventTag)
    event.flags = CGEventFlags()
    switch path {
    case .hid: event.post(tap: .cghidEventTap)
    case .session: event.post(tap: .cgSessionEventTap)
    case .pid: event.postToPid(pid)
    }
}

print("pid=\(pid)")
print("tap=\(path.rawValue)")

// MARK: Action

switch action {
case .click(let button):
    let downType: CGEventType = button == .left ? .leftMouseDown : .rightMouseDown
    let upType: CGEventType = button == .left ? .leftMouseUp : .rightMouseUp
    guard let point = cursor() else { refuse("no_cursor_position") }
    guard
        let down = CGEvent(mouseEventSource: source, mouseType: downType, mouseCursorPosition: point, mouseButton: button),
        let up = CGEvent(mouseEventSource: source, mouseType: upType, mouseCursorPosition: point, mouseButton: button)
    else { refuse("no_mouse_event") }
    for event in [down, up] { event.setIntegerValueField(.mouseEventClickState, value: 1) }
    print("action=click_\(button == .left ? "left" : "right")")
    // Checked again at the instant before the post, not only at start-up.
    guard targetIsFront(), cursorIsOverTarget() else { refuse("focus_or_cursor_changed_before_down") }
    post(down)
    // A hold ends early when the target loses focus; the up is posted either way.
    let holdEnd = Date().addingTimeInterval(Double(holdMs) / 1000)
    var heldToEnd = true
    while Date() < holdEnd {
        if !targetIsFront() { heldToEnd = false; break }
        Thread.sleep(forTimeInterval: 0.01)
    }
    post(up)  // posted even if focus was lost: a stuck button is worse than a stray up
    print("button_down_posted=true")
    print("button_up_posted=true")
    print("hold_ms=\(holdMs)")
    print("held_to_end=\(heldToEnd)")
    print("focus_after=\(targetIsFront())")

case .look(let dx, let dy):
    print("action=look dx=\(dx) dy=\(dy) steps=\(lookSteps)")
    var sentX = 0
    var sentY = 0
    var steps = 0
    for step in 1...lookSteps {
        guard targetIsFront(), let point = cursor() else {
            print("steps_posted=\(steps)")
            print("result=aborted")
            print("reason=focus_lost")
            exit(3)
        }
        // Integer deltas that sum exactly to (dx, dy).
        let stepX = dx * step / lookSteps - sentX
        let stepY = dy * step / lookSteps - sentY
        sentX += stepX
        sentY += stepY
        guard
            let move = CGEvent(
                mouseEventSource: source, mouseType: .mouseMoved,
                mouseCursorPosition: point, mouseButton: .left)
        else { refuse("no_mouse_event") }
        move.setIntegerValueField(.mouseEventDeltaX, value: Int64(stepX))
        move.setIntegerValueField(.mouseEventDeltaY, value: Int64(stepY))
        post(move)
        steps += 1
        Thread.sleep(forTimeInterval: lookStepSeconds)
    }
    print("steps_posted=\(steps)")
    print("delta_sent=\(sentX),\(sentY)")
}

print("note=posted_events_do_not_prove_the_game_accepted_them")
print("result=posted")
