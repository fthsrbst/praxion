//! E13 knobs, all process-local (no system settings). `HANDS_TIMER_MODE` is a comma list of:
//! `nothrottle` (opt out of power throttling incl. timer-resolution throttling),
//! `high` (HIGH_PRIORITY_CLASS + THREAD_PRIORITY_TIME_CRITICAL), `mmcss` (MMCSS task "Games").

use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
    SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};

// Declared here: windows-sys needs dlltool for LibraryLoader, which the self-contained mingw lacks.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut core::ffi::c_void;
    fn GetProcAddress(module: *mut core::ffi::c_void, name: *const u8) -> Option<unsafe extern "system" fn() -> isize>;
}

type AvSetMmThreadCharacteristicsW = unsafe extern "system" fn(*const u16, *mut u32) -> *mut core::ffi::c_void;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// avrt.dll is loaded at run time: the self-contained mingw linker ships no import library for it.
unsafe fn av_set_mm_thread_characteristics(task: &str) -> bool {
    let dll = wide("avrt.dll");
    let task = wide(task);
    unsafe {
        let module = LoadLibraryW(dll.as_ptr());
        if module.is_null() {
            return false;
        }
        let Some(proc) = GetProcAddress(module, c"AvSetMmThreadCharacteristicsW".as_ptr().cast()) else {
            return false;
        };
        let f: AvSetMmThreadCharacteristicsW = std::mem::transmute(proc);
        let mut index = 0u32;
        !f(task.as_ptr(), &mut index).is_null()
    }
}

pub fn apply(mode: &str) {
    for part in mode.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let ok = unsafe {
            match part {
                "nothrottle" => {
                    let state = PROCESS_POWER_THROTTLING_STATE {
                        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                        StateMask: 0,
                    };
                    SetProcessInformation(
                        GetCurrentProcess(),
                        ProcessPowerThrottling,
                        &state as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
                    ) != 0
                }
                "high" => {
                    SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS) != 0
                        && SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) != 0
                }
                "mmcss" => av_set_mm_thread_characteristics("Games"),
                other => {
                    eprintln!("unknown HANDS_TIMER_MODE part: {other}");
                    false
                }
            }
        };
        println!("timer mode {part}: {}", if ok { "ok" } else { "FAILED" });
    }
}
