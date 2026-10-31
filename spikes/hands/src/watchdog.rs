//! Watchdog child process (S6).
//!
//! The injecting parent feeds one line per held-state change on stdin:
//!
//! ```text
//! K <scancode-hex> <ext 0|1> <down 0|1>
//! B <mouse-up-flag-hex> <down 0|1>
//! ```
//!
//! The watchdog waits on the parent process handle; when the parent dies
//! (including `taskkill /F`, a crash or an abort) it sends key-up/button-up for
//! everything the parent last reported holding. Order per E12: movement keys
//! and buttons first, sneak (0x2A left shift) last.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEINPUT, SendInput,
};

const SYNCHRONIZE: u32 = 0x0010_0000;
const INFINITE: u32 = 0xFFFF_FFFF;
const SNEAK_SCANCODE: u16 = 0x2A;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchdogMsg {
    Key { scan: u16, ext: bool, down: bool },
    Button { up_flag: u32, down: bool },
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn parse_hex_u32(s: &str) -> Option<u32> {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

/// Parse one line of the parent -> watchdog protocol.
pub fn parse_line(line: &str) -> Option<WatchdogMsg> {
    let mut parts = line.split_whitespace();
    match parts.next()? {
        "K" => {
            let scan = parse_hex_u32(parts.next()?)?;
            let ext = parse_bool(parts.next()?)?;
            let down = parse_bool(parts.next()?)?;
            if parts.next().is_some() || scan > u16::MAX as u32 {
                return None;
            }
            Some(WatchdogMsg::Key {
                scan: scan as u16,
                ext,
                down,
            })
        }
        "B" => {
            let up_flag = parse_hex_u32(parts.next()?)?;
            let down = parse_bool(parts.next()?)?;
            if parts.next().is_some() {
                return None;
            }
            Some(WatchdogMsg::Button { up_flag, down })
        }
        _ => None,
    }
}

#[derive(Default, Debug)]
pub struct HeldState {
    keys: BTreeSet<(u16, bool)>,
    buttons: BTreeSet<u32>,
}

impl HeldState {
    pub fn apply(&mut self, msg: WatchdogMsg) {
        match msg {
            WatchdogMsg::Key { scan, ext, down } => {
                if down {
                    self.keys.insert((scan, ext));
                } else {
                    self.keys.remove(&(scan, ext));
                }
            }
            WatchdogMsg::Button { up_flag, down } => {
                if down {
                    self.buttons.insert(up_flag);
                } else {
                    self.buttons.remove(&up_flag);
                }
            }
        }
    }

    /// Movement keys first, then buttons, then sneak last.
    pub fn release_sequence(&self) -> Vec<WatchdogMsg> {
        let mut seq = Vec::new();
        for (scan, ext) in self
            .keys
            .iter()
            .filter(|(scan, ext)| !(*scan == SNEAK_SCANCODE && !*ext))
        {
            seq.push(WatchdogMsg::Key {
                scan: *scan,
                ext: *ext,
                down: false,
            });
        }
        for up_flag in &self.buttons {
            seq.push(WatchdogMsg::Button {
                up_flag: *up_flag,
                down: false,
            });
        }
        for (scan, ext) in self
            .keys
            .iter()
            .filter(|(scan, ext)| *scan == SNEAK_SCANCODE && !*ext)
        {
            seq.push(WatchdogMsg::Key {
                scan: *scan,
                ext: *ext,
                down: false,
            });
        }
        seq
    }
}

fn send_key_up(scan: u16, ext: bool) {
    let flags = KEYEVENTF_SCANCODE
        | KEYEVENTF_KEYUP
        | if ext { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: crate::safety::NONCE as usize,
            },
        },
    };
    unsafe {
        let _ = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_button_up(flag: u32) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flag,
                time: 0,
                dwExtraInfo: crate::safety::NONCE as usize,
            },
        },
    };
    unsafe {
        let _ = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn run(pid: u32) -> Result<i32, String> {
    if pid == 0 {
        return Err("pid required".to_string());
    }
    let parent = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if parent.is_null() {
        return Err("cannot open parent process".to_string());
    }

    let state = Arc::new(Mutex::new(HeldState::default()));
    let reader_state = Arc::clone(&state);
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if let Some(msg) = parse_line(&l) {
                        reader_state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .apply(msg);
                    }
                }
                Err(_) => break,
            }
        }
    });

    unsafe {
        WaitForSingleObject(parent as HANDLE, INFINITE);
    }
    let detected = crate::now_ns_pub();
    unsafe {
        CloseHandle(parent);
    }

    // Give the stdin reader a moment to consume the last flushed lines.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let sequence = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .release_sequence();
    let count = sequence.len();
    for msg in sequence {
        match msg {
            WatchdogMsg::Key { scan, ext, .. } => send_key_up(scan, ext),
            WatchdogMsg::Button { up_flag, .. } => send_button_up(up_flag),
        }
    }
    let sent = crate::now_ns_pub();
    let latency_ns = sent.saturating_sub(detected);
    println!(
        "{{\"event\":\"watchdog_released\",\"detected_ns\":{},\"sent_ns\":{},\"latency_ns\":{},\"released\":{}}}",
        detected, sent, latency_ns, count
    );
    crate::log::line(&format!(
        "{{\"t\":{},\"event\":\"watchdog_released\",\"detected_ns\":{},\"sent_ns\":{},\"latency_ns\":{},\"released\":{}}}",
        sent, detected, sent, latency_ns, count
    ));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_lines() {
        assert_eq!(
            parse_line("K 11 0 1"),
            Some(WatchdogMsg::Key {
                scan: 0x11,
                ext: false,
                down: true
            })
        );
        assert_eq!(
            parse_line("K 4B 1 0"),
            Some(WatchdogMsg::Key {
                scan: 0x4B,
                ext: true,
                down: false
            })
        );
        assert_eq!(
            parse_line("K 0x2A 0 1"),
            Some(WatchdogMsg::Key {
                scan: 0x2A,
                ext: false,
                down: true
            })
        );
    }

    #[test]
    fn parse_button_lines() {
        assert_eq!(
            parse_line("B 4 1"),
            Some(WatchdogMsg::Button {
                up_flag: 4,
                down: true
            })
        );
        assert_eq!(
            parse_line("B 10 0"),
            Some(WatchdogMsg::Button {
                up_flag: 0x10,
                down: false
            })
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("K 11 0"), None);
        assert_eq!(parse_line("K 11 2 1"), None);
        assert_eq!(parse_line("K 11 0 1 extra"), None);
        assert_eq!(parse_line("B"), None);
        assert_eq!(parse_line("X 1 2 3"), None);
        assert_eq!(parse_line("K 10000 0 1"), None); // > u16
    }

    #[test]
    fn release_puts_sneak_last() {
        let mut s = HeldState::default();
        s.apply(WatchdogMsg::Key {
            scan: SNEAK_SCANCODE,
            ext: false,
            down: true,
        });
        s.apply(WatchdogMsg::Key {
            scan: 0x11,
            ext: false,
            down: true,
        });
        s.apply(WatchdogMsg::Button {
            up_flag: 0x0004,
            down: true,
        });
        let seq = s.release_sequence();
        assert_eq!(seq.len(), 3);
        assert_eq!(
            seq[0],
            WatchdogMsg::Key {
                scan: 0x11,
                ext: false,
                down: false
            }
        );
        assert!(matches!(seq[1], WatchdogMsg::Button { .. }));
        assert_eq!(
            seq[2],
            WatchdogMsg::Key {
                scan: SNEAK_SCANCODE,
                ext: false,
                down: false
            }
        );
        // Released keys stop being reported as held.
        s.apply(WatchdogMsg::Key {
            scan: 0x11,
            ext: false,
            down: false,
        });
        assert_eq!(s.release_sequence().len(), 2);
    }
}
