//! Safety layer for input injection.
//!
//! Rules that shape this file:
//! - The low-level hook procedures (S5) do no I/O, no allocation and take no
//!   locks. They only fill a fixed `HookEvent` and push it into a lock-free
//!   single-producer ring; a separate logger thread formats and writes it.
//! - Held keys live in fixed arrays of atomics, so the release path
//!   never touches a `Mutex` (S1/S8). Panic and Ctrl+C can therefore release
//!   even while a send is in flight.
//! - A small spin gate serialises the injection thread and the release path.
//!   It is not a `Mutex`: a panic hook can re-enter it without deadlocking
//!   (it notices that the current thread owns the gate).

use std::cell::UnsafeCell;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicIsize, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize,
    Ordering,
};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, SetWaitableTimer,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetWindowTextW, GetWindowThreadProcessId, KBDLLHOOKSTRUCT, KillTimer,
    MSLLHOOKSTRUCT, MSG, SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_MOUSEMOVE, WM_QUIT, WM_TIMER,
};

pub const NONCE: u32 = 0x50524158;

// Compile-time nonce rules from the brief: bit 0x80 clear and never in SDL's
// 0xFF5157xx range.
const _: () = assert!(NONCE & 0x80 == 0);
const _: () = assert!((NONCE & 0xFFFFFF00) != 0xFF515700);

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TIMER_ID: usize = 1;
const MAX_HELD_KEYS: usize = 32;
const SLOT_FREE: u8 = 0;
const SLOT_CLAIMED: u8 = 1;
const SLOT_HELD: u8 = 2;
const SLICE_NS: u64 = 5_000_000; // S3/D5: every wait slice is <= 5 ms
const HEARTBEAT_STALE_NS: u64 = 200_000_000; // S4
const HOOK_RING_CAP: usize = 2048; // power of two

static JITTER_PX: AtomicI32 = AtomicI32::new(3);
static DRIFT_PX: AtomicI32 = AtomicI32::new(20);

static ABORT_FLAG: AtomicBool = AtomicBool::new(false);
static FIRST_PHYSICAL_NS: AtomicU64 = AtomicU64::new(0);
static FIRST_PHYSICAL_KIND: AtomicU8 = AtomicU8::new(0);
static FIRST_PHYSICAL_MSG: AtomicU32 = AtomicU32::new(0);
static FIRST_PHYSICAL_VK: AtomicU32 = AtomicU32::new(0);
static FIRST_PHYSICAL_SCAN: AtomicU32 = AtomicU32::new(0);
static FIRST_PHYSICAL_X: AtomicI32 = AtomicI32::new(0);
static FIRST_PHYSICAL_Y: AtomicI32 = AtomicI32::new(0);
static FIRST_PHYSICAL_RULE: AtomicU8 = AtomicU8::new(0);

static RELEASING: AtomicBool = AtomicBool::new(false);
// 0 = free, otherwise the owning thread id. One atomic carries both the lock
// and the owner, so the panic path can always tell whether re-entry would
// deadlock (S8).
static GATE_OWNER: AtomicU32 = AtomicU32::new(0);

// S1/S8: fixed atomic held-key set. State transitions are FREE -> CLAIMED ->
// HELD; the release path swaps any non-FREE slot back to FREE and sends the up.
static KEY_STATE: [AtomicU8; MAX_HELD_KEYS] = [const { AtomicU8::new(SLOT_FREE) }; MAX_HELD_KEYS];
static KEY_SCAN: [AtomicU16; MAX_HELD_KEYS] = [const { AtomicU16::new(0) }; MAX_HELD_KEYS];
static KEY_EXT: [AtomicBool; MAX_HELD_KEYS] = [const { AtomicBool::new(false) }; MAX_HELD_KEYS];

static STOP_HOOKS: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static HOOKS_READY: AtomicBool = AtomicBool::new(false);
static KH_HANDLE: AtomicIsize = AtomicIsize::new(0);
static MH_HANDLE: AtomicIsize = AtomicIsize::new(0);
static HOOK_ABORT_ENABLED: AtomicBool = AtomicBool::new(false);
static HOOK_LOGGING: AtomicBool = AtomicBool::new(false);
static HOOK_LOG_START: AtomicU64 = AtomicU64::new(0);
static HOOK_HEARTBEAT_NS: AtomicU64 = AtomicU64::new(0);

// Mouse abort state (read/written by the hook, i.e. plain atomics).
static PREV_MOUSE_X: AtomicI32 = AtomicI32::new(0);
static PREV_MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static PREV_MOUSE_VALID: AtomicBool = AtomicBool::new(false);
static MOUSE_BASELINE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_BASELINE_Y: AtomicI32 = AtomicI32::new(0);
static MOUSE_BASELINE_VALID: AtomicBool = AtomicBool::new(false);

// hook-log movement samples, written by the logger thread only.
static MOUSE_SAMPLES: Mutex<Vec<(usize, f64)>> = Mutex::new(Vec::new());
static HOOK_LOGGER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static HOOK_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

// Watchdog pipe: the injecting process reports held scancodes/buttons here.
static WATCHDOG_PIPE: Mutex<Option<Box<dyn std::io::Write + Send>>> = Mutex::new(None);
static WATCHDOG_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Lock-free single-producer / single-consumer ring (S5)
// ---------------------------------------------------------------------------

/// One hook event, fixed size and `Copy` so the ring never allocates.
#[derive(Clone, Copy)]
pub struct HookEvent {
    pub t: u64,
    pub kind: u8,
    pub injected: bool,
    pub msg: u32,
    pub vk: u32,
    pub scan: u32,
    pub flags: u32,
    pub hook_ts: u32,
    pub extra: u64,
    pub x: i32,
    pub y: i32,
    pub mouse_data: u32,
}

impl HookEvent {
    pub const EMPTY: HookEvent = HookEvent {
        t: 0,
        kind: 0,
        injected: false,
        msg: 0,
        vk: 0,
        scan: 0,
        flags: 0,
        hook_ts: 0,
        extra: 0,
        x: 0,
        y: 0,
        mouse_data: 0,
    };
}

struct HookRing {
    head: AtomicUsize,
    tail: AtomicUsize,
    // Safety: single producer (hook thread) writes `tail`, single consumer
    // (logger thread) writes `head`; slots in between are not touched by both.
    buf: UnsafeCell<[HookEvent; HOOK_RING_CAP]>,
}

unsafe impl Sync for HookRing {}

impl HookRing {
    const fn new() -> Self {
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            buf: UnsafeCell::new([HookEvent::EMPTY; HOOK_RING_CAP]),
        }
    }

    fn push(&self, ev: HookEvent) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= HOOK_RING_CAP {
            return false;
        }
        // SAFETY: the producer owns `tail & (CAP-1)` until it publishes `tail`.
        unsafe {
            (*self.buf.get())[tail & (HOOK_RING_CAP - 1)] = ev;
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop(&self) -> Option<HookEvent> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        // SAFETY: the consumer owns `head & (CAP-1)` after observing `tail`.
        let ev = unsafe { (*self.buf.get())[head & (HOOK_RING_CAP - 1)] };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(ev)
    }
}

static HOOK_RING: HookRing = HookRing::new();

fn pop_hook_event() -> Option<HookEvent> {
    HOOK_RING.pop()
}

// ---------------------------------------------------------------------------
// Initialisation, panic hook, Ctrl+C
// ---------------------------------------------------------------------------

pub fn init(jitter_px: i32, drift_px: i32) {
    JITTER_PX.store(jitter_px.max(0), Ordering::SeqCst);
    DRIFT_PX.store(drift_px.max(0), Ordering::SeqCst);

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // S8: release_all uses atomics only, so this cannot deadlock on a
        // poisoned mutex held by the panicking thread.
        release_all();
        default_hook(info);
    }));

    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }
}

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> i32 {
    release_all();
    std::process::exit(130);
}

fn current_thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}

fn lock_gate() {
    let me = current_thread_id();
    loop {
        if GATE_OWNER
            .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        std::hint::spin_loop();
    }
}

fn unlock_gate() {
    GATE_OWNER.store(0, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Foreground guard (S7)
// ---------------------------------------------------------------------------

pub fn foreground_ok(prefix: &str) -> bool {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), 512);
        if len <= 0 {
            return false;
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        if !title.starts_with(prefix) {
            return false;
        }
        // S7: the owning process must be java.exe / javaw.exe.
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return false;
        }
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if proc.is_null() {
            return false;
        }
        let mut path_buf = [0u16; 512];
        let mut path_len = path_buf.len() as u32;
        let res = QueryFullProcessImageNameW(proc, 0, path_buf.as_mut_ptr(), &mut path_len);
        let _ = CloseHandle(proc);
        if res == 0 {
            return false;
        }
        let path = String::from_utf16_lossy(&path_buf[..path_len as usize]);
        let lower = path.to_lowercase();
        lower.ends_with("java.exe") || lower.ends_with("javaw.exe")
    }
}

// ---------------------------------------------------------------------------
// Abort state
// ---------------------------------------------------------------------------

pub fn abort() -> bool {
    ABORT_FLAG.load(Ordering::SeqCst)
}

pub fn first_physical_time() -> u64 {
    FIRST_PHYSICAL_NS.load(Ordering::SeqCst)
}

fn rule_name(code: u8) -> &'static str {
    match code {
        1 => "jump",
        2 => "drift",
        3 => "button",
        _ => "none",
    }
}

/// Build the identity of the first physical event after the fact, so the hook
/// itself never formats or allocates (S5/M3/D3).
pub fn first_physical_event() -> String {
    if FIRST_PHYSICAL_NS.load(Ordering::SeqCst) == 0 {
        return "null".to_string();
    }
    match FIRST_PHYSICAL_KIND.load(Ordering::SeqCst) {
        1 => format!(
            "{{\"type\":\"keyboard\",\"vk\":{},\"scancode\":{}}}",
            FIRST_PHYSICAL_VK.load(Ordering::SeqCst),
            FIRST_PHYSICAL_SCAN.load(Ordering::SeqCst)
        ),
        2 => format!(
            "{{\"type\":\"mouse\",\"msg\":{},\"point\":{{\"x\":{},\"y\":{}}},\"rule\":\"{}\"}}",
            FIRST_PHYSICAL_MSG.load(Ordering::SeqCst),
            FIRST_PHYSICAL_X.load(Ordering::SeqCst),
            FIRST_PHYSICAL_Y.load(Ordering::SeqCst),
            rule_name(FIRST_PHYSICAL_RULE.load(Ordering::SeqCst))
        ),
        3 => format!(
            "{{\"type\":\"mouse\",\"msg\":{},\"point\":{{\"x\":{},\"y\":{}}},\"rule\":\"button\"}}",
            FIRST_PHYSICAL_MSG.load(Ordering::SeqCst),
            FIRST_PHYSICAL_X.load(Ordering::SeqCst),
            FIRST_PHYSICAL_Y.load(Ordering::SeqCst)
        ),
        _ => "null".to_string(),
    }
}

fn note_first_physical(
    kind: u8,
    msg: u32,
    vk: u32,
    scan: u32,
    x: i32,
    y: i32,
    rule: u8,
    t: u64,
) {
    if FIRST_PHYSICAL_NS.load(Ordering::SeqCst) == 0 {
        FIRST_PHYSICAL_KIND.store(kind, Ordering::SeqCst);
        FIRST_PHYSICAL_MSG.store(msg, Ordering::SeqCst);
        FIRST_PHYSICAL_VK.store(vk, Ordering::SeqCst);
        FIRST_PHYSICAL_SCAN.store(scan, Ordering::SeqCst);
        FIRST_PHYSICAL_X.store(x, Ordering::SeqCst);
        FIRST_PHYSICAL_Y.store(y, Ordering::SeqCst);
        FIRST_PHYSICAL_RULE.store(rule, Ordering::SeqCst);
        FIRST_PHYSICAL_NS.store(t, Ordering::SeqCst);
    }
}

/// Pure rule used by the mouse hook and tested directly (M2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseRule {
    Jump,
    Drift,
}

pub fn mouse_abort_rule(
    prev: (i32, i32),
    baseline: (i32, i32),
    cur: (i32, i32),
    jitter_px: i32,
    drift_px: i32,
) -> Option<MouseRule> {
    let dx_jump = (cur.0 - prev.0).abs();
    let dy_jump = (cur.1 - prev.1).abs();
    if jitter_px >= 0 && (dx_jump > jitter_px || dy_jump > jitter_px) {
        return Some(MouseRule::Jump);
    }
    let dx = (cur.0 - baseline.0) as f64;
    let dy = (cur.1 - baseline.1) as f64;
    let distance = (dx * dx + dy * dy).sqrt();
    if drift_px >= 0 && distance > drift_px as f64 {
        return Some(MouseRule::Drift);
    }
    None
}

// ---------------------------------------------------------------------------
// Low-level SendInput wrappers
// ---------------------------------------------------------------------------

fn send_key_raw(scan: u16, ext: bool, down: bool) -> Result<(), u32> {
    let flags = KEYEVENTF_SCANCODE
        | if down { 0 } else { KEYEVENTF_KEYUP }
        | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: NONCE as usize,
            },
        },
    };
    let res = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if res == 1 { Ok(()) } else { Err(unsafe { GetLastError() }) }
}

fn send_mouse_move_raw(dx: i32, dy: i32) -> Result<(), u32> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: NONCE as usize,
            },
        },
    };
    let res = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if res == 1 { Ok(()) } else { Err(unsafe { GetLastError() }) }
}

/// S9: a failed send is fatal, but everything held is released first.
fn fail_send(err: u32) -> ! {
    crate::log::line(&format!(
        "{{\"t\":{},\"event\":\"send_failed\",\"err\":{}}}",
        crate::now_ns_pub(),
        err
    ));
    eprintln!("send_failed err={}", err);
    release_all();
    std::process::exit(6);
}

fn report_to_watchdog(msg: &str) {
    if let Ok(mut g) = WATCHDOG_PIPE.lock() {
        if let Some(w) = g.as_mut() {
            let _ = writeln!(w, "{}", msg);
            let _ = w.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Held-key / held-button sets (S1/S8)
// ---------------------------------------------------------------------------

fn find_key_slot(scan: u16, ext: bool) -> Option<usize> {
    for i in 0..MAX_HELD_KEYS {
        if KEY_STATE[i].load(Ordering::Acquire) != SLOT_FREE
            && KEY_SCAN[i].load(Ordering::Relaxed) == scan
            && KEY_EXT[i].load(Ordering::Relaxed) == ext
        {
            return Some(i);
        }
    }
    None
}

fn claim_key_slot(scan: u16, ext: bool) -> Option<usize> {
    if let Some(i) = find_key_slot(scan, ext) {
        return Some(i);
    }
    for i in 0..MAX_HELD_KEYS {
        if KEY_STATE[i]
            .compare_exchange(SLOT_FREE, SLOT_CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            KEY_SCAN[i].store(scan, Ordering::Relaxed);
            KEY_EXT[i].store(ext, Ordering::Relaxed);
            return Some(i);
        }
    }
    None
}

fn take_key_slot(i: usize) -> Option<(u16, bool)> {
    if KEY_STATE[i].swap(SLOT_FREE, Ordering::AcqRel) == SLOT_FREE {
        return None;
    }
    Some((
        KEY_SCAN[i].load(Ordering::Relaxed),
        KEY_EXT[i].load(Ordering::Relaxed),
    ))
}

fn is_sneak(scan: u16, ext: bool) -> bool {
    // 0x2A = left shift, the danger-held key in E12.
    scan == 0x2A && !ext
}

/// S1: refuse key-down once release started, and keep the claim + SendInput +
/// held-mark atomic with respect to `release_all` via the gate. Returns the
/// post-SendInput timestamp (M7's `*_done_t`), or `None` if the send was
/// refused.
pub fn send_key(scan: u16, ext: bool, down: bool) -> Option<u64> {
    if down && abort() {
        return None;
    }
    if down && check_hook_heartbeat_stale() {
        release_all();
        return None;
    }

    lock_gate();
    if down && (RELEASING.load(Ordering::SeqCst) || abort()) {
        unlock_gate();
        return None;
    }

    let mut fail = None;
    if down {
        if claim_key_slot(scan, ext).is_none() {
            unlock_gate();
            return None;
        }
        // Report before sending: a key-up for a key that never went down is
        // harmless, a missed key-down is not.
        report_to_watchdog(&format!("K {:X} {} 1", scan, u8::from(ext)));
        if let Err(e) = send_key_raw(scan, ext, true) {
            fail = Some(e);
        } else if let Some(i) = find_key_slot(scan, ext) {
            KEY_STATE[i].store(SLOT_HELD, Ordering::Release);
        }
    } else {
        if let Some(i) = find_key_slot(scan, ext) {
            KEY_STATE[i].store(SLOT_FREE, Ordering::Release);
        }
        if let Err(e) = send_key_raw(scan, ext, false) {
            fail = Some(e);
        }
        report_to_watchdog(&format!("K {:X} {} 0", scan, u8::from(ext)));
    }
    let done = crate::now_ns_pub();
    unlock_gate();

    if let Some(e) = fail {
        fail_send(e);
    }
    Some(done)
}

/// S2: release a key regardless of the abort/releasing gates. Used by
/// `takeover-test` so a logged "release" really sent the key-up.
pub fn release_key_now(scan: u16, ext: bool) {
    lock_gate();
    if let Some(i) = find_key_slot(scan, ext) {
        KEY_STATE[i].store(SLOT_FREE, Ordering::Release);
    }
    let _ = send_key_raw(scan, ext, false);
    report_to_watchdog(&format!("K {:X} {} 0", scan, u8::from(ext)));
    unlock_gate();
}

pub fn send_mouse_move(dx: i32, dy: i32) -> Option<u64> {
    if abort() {
        return None;
    }
    if check_hook_heartbeat_stale() {
        release_all();
        return None;
    }
    lock_gate();
    if RELEASING.load(Ordering::SeqCst) || abort() {
        unlock_gate();
        return None;
    }
    let res = send_mouse_move_raw(dx, dy);
    let done = crate::now_ns_pub();
    unlock_gate();
    if let Err(e) = res {
        fail_send(e);
    }
    Some(done)
}

/// Release everything the process holds. Order per E12: movement keys first,
/// sneak (0x2A) last. No locks are taken, so this is safe from the panic hook
/// and the Ctrl+C handler (S1/S8).
pub fn release_all() {
    RELEASING.store(true, Ordering::SeqCst);
    let own_gate = GATE_OWNER.load(Ordering::Relaxed) == current_thread_id();
    if !own_gate {
        lock_gate();
    }

    let mut normal: Vec<(u16, bool)> = Vec::new();
    let mut sneak: Vec<(u16, bool)> = Vec::new();
    for i in 0..MAX_HELD_KEYS {
        if let Some((scan, ext)) = take_key_slot(i) {
            if is_sneak(scan, ext) {
                sneak.push((scan, ext));
            } else {
                normal.push((scan, ext));
            }
        }
    }
    // No watchdog reports here: the release path must stay free of the pipe
    // mutex (panic/Ctrl+C). The watchdog releases on its own when the parent
    // process ends, which is exactly when these up events matter.
    for (scan, ext) in normal {
        let _ = send_key_raw(scan, ext, false);
    }
    for (scan, ext) in sneak {
        let _ = send_key_raw(scan, ext, false);
    }

    // Only the call that took the gate gives it back; a re-entrant call (panic while holding it)
    // must not hand the gate to another thread.
    if !own_gate {
        unlock_gate();
    }
}

pub fn check_hook_heartbeat_stale() -> bool {
    let last = HOOK_HEARTBEAT_NS.load(Ordering::SeqCst);
    if last == 0 {
        return false;
    }
    crate::now_ns_pub().saturating_sub(last) > HEARTBEAT_STALE_NS
}

// ---------------------------------------------------------------------------
// Waitable timer + sliced waits (S3/D5/D1/D2, M8)
// ---------------------------------------------------------------------------

pub fn create_waitable_timer_highres() -> isize {
    unsafe {
        windows_sys::Win32::System::Threading::CreateWaitableTimerExW(
            std::ptr::null(),
            std::ptr::null(),
            windows_sys::Win32::System::Threading::CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            0x1F0003,
        ) as isize
    }
}

pub fn close_handle(h: isize) {
    if h != 0 {
        unsafe {
            CloseHandle(h as HANDLE);
        }
    }
}

/// Sleep for at most `ns` using the high-resolution timer. Bounded on every
/// failure path (M8): a failed `SetWaitableTimer` falls back to a short sleep.
fn sleep_ns(timer: isize, ns: u64) {
    if ns == 0 {
        return;
    }
    if timer == 0 {
        std::thread::sleep(std::time::Duration::from_nanos(ns));
        return;
    }
    let due: i64 = -((ns / 100).max(1) as i64);
    let ok = unsafe {
        SetWaitableTimer(
            timer as HANDLE,
            &due,
            0,
            None,
            std::ptr::null(),
            0,
        )
    };
    if ok != 0 {
        let ms = (ns / 1_000_000) as u32 + 2;
        unsafe {
            WaitForSingleObject(timer as HANDLE, ms);
        }
    } else {
        std::thread::sleep(std::time::Duration::from_nanos(ns));
    }
}

/// Wait until an absolute deadline, waking at least every 5 ms and looking for
/// an abort / foreground / heartbeat / max-secs problem each slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Done,
    Aborted,
    ForegroundLost,
    Timeout,
    HookDead,
}

pub struct Slicer {
    timer: isize,
}

impl Slicer {
    pub fn new() -> Self {
        Self {
            timer: create_waitable_timer_highres(),
        }
    }

    pub fn wait(
        &self,
        deadline_ns: u64,
        program_start: u64,
        max_secs: u64,
        prefix: &str,
    ) -> WaitOutcome {
        self.wait_impl(deadline_ns, program_start, max_secs, Some(prefix))
    }

    fn wait_impl(
        &self,
        deadline_ns: u64,
        program_start: u64,
        max_secs: u64,
        foreground: Option<&str>,
    ) -> WaitOutcome {
        loop {
            let now = crate::now_ns_pub();
            if abort() {
                return WaitOutcome::Aborted;
            }
            if check_hook_heartbeat_stale() {
                return WaitOutcome::HookDead;
            }
            if let Some(prefix) = foreground {
                if !foreground_ok(prefix) {
                    return WaitOutcome::ForegroundLost;
                }
            }
            if max_secs > 0 && now.saturating_sub(program_start) >= max_secs * 1_000_000_000 {
                return WaitOutcome::Timeout;
            }
            let rem = deadline_ns.saturating_sub(now);
            if rem == 0 {
                return WaitOutcome::Done;
            }
            sleep_ns(self.timer, rem.min(SLICE_NS));
        }
    }
}

impl Drop for Slicer {
    fn drop(&mut self) {
        close_handle(self.timer);
    }
}

/// Absolute-deadline wait for the non-injecting jitter loop.
pub fn wait_until_highres(handle: isize, deadline_ns: u64) {
    loop {
        let now = crate::now_ns_pub();
        if now >= deadline_ns {
            return;
        }
        sleep_ns(handle, deadline_ns - now);
    }
}

// ---------------------------------------------------------------------------
// Hook thread, hooks (S4, S5)
// ---------------------------------------------------------------------------

fn start_hooks_inner(logging: bool) {
    HOOK_ABORT_ENABLED.store(!logging, Ordering::SeqCst);
    HOOK_LOGGING.store(logging, Ordering::SeqCst);
    ABORT_FLAG.store(false, Ordering::SeqCst);
    FIRST_PHYSICAL_NS.store(0, Ordering::SeqCst);
    FIRST_PHYSICAL_KIND.store(0, Ordering::SeqCst);
    PREV_MOUSE_VALID.store(false, Ordering::SeqCst);
    MOUSE_BASELINE_VALID.store(false, Ordering::SeqCst);
    STOP_HOOKS.store(false, Ordering::SeqCst);
    RELEASING.store(false, Ordering::SeqCst);
    HOOKS_READY.store(false, Ordering::SeqCst);
    KH_HANDLE.store(0, Ordering::SeqCst);
    MH_HANDLE.store(0, Ordering::SeqCst);
    MOUSE_SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let start = crate::now_ns_pub();
    HOOK_LOG_START.store(if logging { start } else { 0 }, Ordering::SeqCst);
    HOOK_HEARTBEAT_NS.store(start, Ordering::SeqCst);

    // M2: baseline is the real cursor position when hooks start, so the very
    // first physical mouse event can already be compared.
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut p) != 0 {
            MOUSE_BASELINE_X.store(p.x, Ordering::SeqCst);
            MOUSE_BASELINE_Y.store(p.y, Ordering::SeqCst);
            MOUSE_BASELINE_VALID.store(true, Ordering::SeqCst);
            PREV_MOUSE_X.store(p.x, Ordering::SeqCst);
            PREV_MOUSE_Y.store(p.y, Ordering::SeqCst);
            PREV_MOUSE_VALID.store(true, Ordering::SeqCst);
        }
    }

    // Logger thread: drains the ring, formats lines, aggregates M4 samples.
    let logger = std::thread::spawn(hook_logger_main);
    *HOOK_LOGGER.lock().unwrap_or_else(|e| e.into_inner()) = Some(logger);

    let hook_thread = std::thread::spawn(hook_thread_main);
    *HOOK_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(hook_thread);

    for _ in 0..200 {
        if HOOKS_READY.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let kh = KH_HANDLE.load(Ordering::SeqCst);
    let mh = MH_HANDLE.load(Ordering::SeqCst);
    if !HOOKS_READY.load(Ordering::SeqCst) || kh == 0 || mh == 0 {
        let err = unsafe { GetLastError() };
        eprintln!("hook install failed: kh={} mh={} err={}", kh, mh, err);
        crate::log::line(&format!(
            "{{\"t\":{},\"event\":\"hook_install_failed\",\"kh\":{},\"mh\":{},\"err\":{}}}",
            crate::now_ns_pub(),
            kh,
            mh,
            err
        ));
        // S4: never inject without a working abort path.
        std::process::exit(5);
    }
    // Confirm the heartbeat timer is live. Without it the injection thread
    // could not tell that Windows silently removed the hook (S4).
    for _ in 0..100 {
        if HOOK_HEARTBEAT_NS.load(Ordering::SeqCst) > start {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if HOOK_HEARTBEAT_NS.load(Ordering::SeqCst) <= start {
        eprintln!("hook heartbeat did not start");
        crate::log::line(&format!(
            "{{\"t\":{},\"event\":\"hook_heartbeat_failed\"}}",
            crate::now_ns_pub()
        ));
        std::process::exit(5);
    }
}

pub fn start_hooks() {
    start_hooks_inner(false);
}

pub fn start_hooks_log() {
    start_hooks_inner(true);
}

pub fn stop_hooks() {
    STOP_HOOKS.store(true, Ordering::SeqCst);
    let tid = HOOK_THREAD_ID.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(tid, WM_QUIT, 0, 0);
        }
    }
    if let Some(h) = HOOK_THREAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = h.join();
    }
    HOOK_THREAD_ID.store(0, Ordering::SeqCst);

    // Stop the logger only after the hook thread is gone; then drain the ring.
    HOOK_LOGGING.store(false, Ordering::SeqCst);
    if let Some(h) = HOOK_LOGGER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = h.join();
    }
}

fn hook_logger_main() {
    let mut prev: Option<(i32, i32)> = None;
    loop {
        let running = HOOK_LOGGING.load(Ordering::SeqCst);
        match pop_hook_event() {
            Some(ev) => log_hook_event(ev, &mut prev),
            None => {
                if !running {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

fn log_hook_event(ev: HookEvent, prev: &mut Option<(i32, i32)>) {
    if ev.kind == 1 {
        crate::log::line(&format!(
            "{{\"t\":{},\"type\":\"keyboard\",\"injected\":{},\"extra\":\"0x{:X}\",\"vk\":{},\"scancode\":{},\"flags\":{},\"hook_ts\":{}}}",
            ev.t, ev.injected, ev.extra, ev.vk, ev.scan, ev.flags, ev.hook_ts
        ));
    } else {
        crate::log::line(&format!(
            "{{\"t\":{},\"type\":\"mouse\",\"msg\":{},\"injected\":{},\"extra\":\"0x{:X}\",\"point\":{{\"x\":{},\"y\":{}}},\"mouse_data\":{},\"flags\":{},\"hook_ts\":{}}}",
            ev.t, ev.msg, ev.injected, ev.extra, ev.x, ev.y, ev.mouse_data, ev.flags, ev.hook_ts
        ));
        // M4: movement magnitude per 100 ms window from consecutive physical
        // pointer positions. Empty windows are filled with 0 by the summary.
        if !ev.injected && ev.msg == WM_MOUSEMOVE as u32 {
            if let Some((px, py)) = *prev {
                let dx = (ev.x - px) as f64;
                let dy = (ev.y - py) as f64;
                let mag = (dx * dx + dy * dy).sqrt();
                let start = HOOK_LOG_START.load(Ordering::SeqCst);
                if start > 0 && ev.t >= start {
                    let window = ((ev.t - start) / 100_000_000) as usize;
                    MOUSE_SAMPLES
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((window, mag));
                }
            }
            *prev = Some((ev.x, ev.y));
        }
    }
}

fn hook_thread_main() {
    HOOK_THREAD_ID.store(current_thread_id(), Ordering::SeqCst);
    unsafe {
        let kh = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), std::ptr::null_mut(), 0);
        let mh = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), std::ptr::null_mut(), 0);
        KH_HANDLE.store(kh as isize, Ordering::SeqCst);
        MH_HANDLE.store(mh as isize, Ordering::SeqCst);

        let mut timer_id: usize = 0;
        if !kh.is_null() && !mh.is_null() {
            // S4: the hook thread posts itself WM_TIMER every 20 ms; handling
            // that message updates the heartbeat, proving this message loop
            // (the thing Windows will silently remove on a timeout) is alive.
            // With a null hwnd Windows ignores the requested id and returns its own;
            // WM_TIMER carries that returned id in wParam.
            timer_id = SetTimer(std::ptr::null_mut(), TIMER_ID, 20, None);
            HOOK_HEARTBEAT_NS.store(crate::now_ns_pub(), Ordering::SeqCst);
        }
        HOOKS_READY.store(true, Ordering::SeqCst);

        if !kh.is_null() && !mh.is_null() {
            let mut msg: MSG = std::mem::zeroed();
            loop {
                if STOP_HOOKS.load(Ordering::SeqCst) {
                    break;
                }
                let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if r == 0 || r == -1 {
                    break;
                }
                if msg.message == WM_TIMER && msg.hwnd.is_null() && msg.wParam == timer_id {
                    HOOK_HEARTBEAT_NS.store(crate::now_ns_pub(), Ordering::SeqCst);
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            KillTimer(std::ptr::null_mut(), TIMER_ID);
        }
        if !kh.is_null() {
            UnhookWindowsHookEx(kh);
        }
        if !mh.is_null() {
            UnhookWindowsHookEx(mh);
        }
    }
    HOOK_THREAD_ID.store(0, Ordering::SeqCst);
}

// Hook procedures: fixed-size work only (S5).

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let injected = (info.flags & 0x0000_0010) != 0; // LLKHF_INJECTED
    let t = crate::now_ns_pub();

    if !injected && HOOK_ABORT_ENABLED.load(Ordering::SeqCst) {
        note_first_physical(1, 0, info.vkCode, info.scanCode, 0, 0, 0, t);
        ABORT_FLAG.store(true, Ordering::SeqCst);
    }

    if HOOK_LOGGING.load(Ordering::SeqCst) {
        HOOK_RING.push(HookEvent {
            t,
            kind: 1,
            injected,
            msg: 0,
            vk: info.vkCode,
            scan: info.scanCode,
            flags: info.flags,
            hook_ts: info.time,
            extra: info.dwExtraInfo as u64,
            x: 0,
            y: 0,
            mouse_data: 0,
        });
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    let injected = (info.flags & 0x0000_0001) != 0; // LLMHF_INJECTED
    let t = crate::now_ns_pub();
    let is_move = wparam as u32 == WM_MOUSEMOVE as u32;

    if !injected && HOOK_ABORT_ENABLED.load(Ordering::SeqCst) {
        if is_move {
            if !MOUSE_BASELINE_VALID.load(Ordering::SeqCst) {
                MOUSE_BASELINE_X.store(info.pt.x, Ordering::SeqCst);
                MOUSE_BASELINE_Y.store(info.pt.y, Ordering::SeqCst);
                MOUSE_BASELINE_VALID.store(true, Ordering::SeqCst);
            }
            if !PREV_MOUSE_VALID.load(Ordering::SeqCst) {
                PREV_MOUSE_X.store(info.pt.x, Ordering::SeqCst);
                PREV_MOUSE_Y.store(info.pt.y, Ordering::SeqCst);
                PREV_MOUSE_VALID.store(true, Ordering::SeqCst);
            }
            let prev = (
                PREV_MOUSE_X.load(Ordering::SeqCst),
                PREV_MOUSE_Y.load(Ordering::SeqCst),
            );
            let baseline = (
                MOUSE_BASELINE_X.load(Ordering::SeqCst),
                MOUSE_BASELINE_Y.load(Ordering::SeqCst),
            );
            let cur = (info.pt.x, info.pt.y);
            // M2: previous point is always updated, exactly as the brief says.
            PREV_MOUSE_X.store(cur.0, Ordering::SeqCst);
            PREV_MOUSE_Y.store(cur.1, Ordering::SeqCst);
            if let Some(rule) = mouse_abort_rule(
                prev,
                baseline,
                cur,
                JITTER_PX.load(Ordering::SeqCst),
                DRIFT_PX.load(Ordering::SeqCst),
            ) {
                let code = match rule {
                    MouseRule::Jump => 1,
                    MouseRule::Drift => 2,
                };
                note_first_physical(2, wparam as u32, 0, 0, cur.0, cur.1, code, t);
                ABORT_FLAG.store(true, Ordering::SeqCst);
            }
        } else {
            note_first_physical(3, wparam as u32, 0, 0, info.pt.x, info.pt.y, 3, t);
            ABORT_FLAG.store(true, Ordering::SeqCst);
        }
    }

    if HOOK_LOGGING.load(Ordering::SeqCst) {
        HOOK_RING.push(HookEvent {
            t,
            kind: 2,
            injected,
            msg: wparam as u32,
            vk: 0,
            scan: 0,
            flags: info.flags,
            hook_ts: info.time,
            extra: info.dwExtraInfo as u64,
            x: info.pt.x,
            y: info.pt.y,
            mouse_data: info.mouseData,
        });
    }

    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

// ---------------------------------------------------------------------------
// hook-log summary (M4)
// ---------------------------------------------------------------------------

pub struct HookLogSummary {
    pub windows_total: usize,
    pub windows_nonempty: usize,
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn hook_log_summary(duration_ns: u64) -> HookLogSummary {
    let samples = MOUSE_SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    // M4: every elapsed 100 ms window counts, including empty ones. The max
    // sample index wins so a sample taken just before stop is never dropped.
    let max_window = samples.iter().map(|(w, _)| *w + 1).max().unwrap_or(0);
    let total = ((duration_ns / 100_000_000) as usize).max(max_window).max(1);
    let mut vals = vec![0.0f64; total];
    let mut seen = vec![false; total];
    let mut nonempty = 0usize;
    for (w, m) in samples.iter() {
        if *w < total {
            if !seen[*w] {
                seen[*w] = true;
                nonempty += 1;
            }
            vals[*w] += m;
        }
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    HookLogSummary {
        windows_total: total,
        windows_nonempty: nonempty,
        p50: crate::percentiles::percentile_f64(&vals, 0.50),
        p99: crate::percentiles::percentile_f64(&vals, 0.99),
        max: crate::percentiles::percentile_f64(&vals, 1.0),
    }
}

// ---------------------------------------------------------------------------
// Watchdog (S6)
// ---------------------------------------------------------------------------

/// Spawn the watchdog child and wire its stdin to the held-state pipe. The
/// `Child` is kept alive in a static so its stdin handle stays valid.
pub fn spawn_watchdog() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let pid = std::process::id();
    let mut child = std::process::Command::new(exe)
        .arg("watchdog")
        .arg("--pid")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("watchdog stdin missing")?;
    *WATCHDOG_PIPE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(stdin));
    *WATCHDOG_CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // The abort flag and heartbeat are process globals; the two sliced-wait
    // tests must not observe each other's changes.
    static WAIT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn nonce_bit_rules() {
        assert_eq!(NONCE & 0x80, 0);
        assert_ne!(NONCE & 0xFFFFFF00, 0xFF515700);
    }

    #[test]
    fn ring_buffer_fifo_and_overflow() {
        // Drain anything left by another test.
        while pop_hook_event().is_some() {}
        for i in 0..3u64 {
            let mut ev = HookEvent::EMPTY;
            ev.t = i;
            assert!(HOOK_RING.push(ev));
        }
        for i in 0..3u64 {
            assert_eq!(pop_hook_event().unwrap().t, i);
        }
        assert!(pop_hook_event().is_none());

        for i in 0..HOOK_RING_CAP {
            let mut ev = HookEvent::EMPTY;
            ev.t = i as u64;
            assert!(HOOK_RING.push(ev));
        }
        assert!(!HOOK_RING.push(HookEvent::EMPTY), "full ring must drop");
        for i in 0..HOOK_RING_CAP {
            assert_eq!(pop_hook_event().unwrap().t, i as u64);
        }
    }

    #[test]
    fn held_key_atomic_set() {
        for i in 0..MAX_HELD_KEYS {
            KEY_STATE[i].store(SLOT_FREE, Ordering::SeqCst);
        }
        let a = claim_key_slot(0x11, false).expect("free slot");
        assert_eq!(find_key_slot(0x11, false), Some(a));
        KEY_STATE[a].store(SLOT_HELD, Ordering::SeqCst);
        // Claiming an already-held key reuses the same slot.
        assert_eq!(claim_key_slot(0x11, false), Some(a));
        // Extended and non-extended are distinct keys.
        let b = claim_key_slot(0x4B, true).expect("second slot");
        assert_ne!(a, b);
        assert_eq!(take_key_slot(a), Some((0x11, false)));
        assert_eq!(find_key_slot(0x11, false), None);
        assert_eq!(take_key_slot(a), None, "second take is a no-op");
        assert_eq!(take_key_slot(b), Some((0x4B, true)));
    }

    #[test]
    fn mouse_abort_rule_jump() {
        // 10 px jump on x from the previous physical point, no drift.
        assert_eq!(
            mouse_abort_rule((0, 0), (0, 0), (10, 0), 3, 20),
            Some(MouseRule::Jump)
        );
        assert_eq!(
            mouse_abort_rule((0, 0), (0, 0), (0, -10), 3, 20),
            Some(MouseRule::Jump)
        );
    }

    #[test]
    fn mouse_abort_rule_drift() {
        // Each step is only 2 px (below jitter) but 25 px total from baseline.
        assert_eq!(
            mouse_abort_rule((23, 0), (0, 0), (25, 0), 3, 20),
            Some(MouseRule::Drift)
        );
        // Under both thresholds: no abort.
        assert_eq!(mouse_abort_rule((10, 10), (0, 0), (12, 11), 3, 20), None);
    }

    #[test]
    fn sliced_wait_aborts_within_10ms() {
        let _guard = WAIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ABORT_FLAG.store(false, Ordering::SeqCst);
        HOOK_HEARTBEAT_NS.store(crate::now_ns_pub(), Ordering::SeqCst);
        let slicer = Slicer::new();
        let setter = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(2));
            ABORT_FLAG.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let deadline = crate::now_ns_pub() + 500_000_000;
        let outcome = slicer.wait_impl(deadline, crate::now_ns_pub(), 0, None);
        let elapsed = start.elapsed();
        let _ = setter.join();
        assert_eq!(outcome, WaitOutcome::Aborted);
        assert!(
            elapsed < Duration::from_millis(10),
            "abort observed after {:?}",
            elapsed
        );
        ABORT_FLAG.store(false, Ordering::SeqCst);
    }

    #[test]
    fn sliced_wait_respects_max_secs() {
        let _guard = WAIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ABORT_FLAG.store(false, Ordering::SeqCst);
        HOOK_HEARTBEAT_NS.store(crate::now_ns_pub(), Ordering::SeqCst);
        let slicer = Slicer::new();
        let now = crate::now_ns_pub();
        // The 1 s program budget was already used 2 s ago, even though the
        // wait deadline is 5 s away: the max-secs deadline wins.
        let outcome = slicer.wait_impl(now + 5_000_000_000, now - 2_000_000_000, 1, None);
        assert_eq!(outcome, WaitOutcome::Timeout);
    }
}
