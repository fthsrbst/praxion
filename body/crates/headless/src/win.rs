//! The body's loop process settings (docs/decisions.md 2026-11-01): opt out of
//! power throttling, `HIGH_PRIORITY_CLASS`, a time-critical loop thread, and the
//! MMCSS "Games" task. Process-local only; no system setting is touched.
//!
//! Free-standing because they belong to the thread that runs the tick loop, not
//! to the connection: the body will use the same call for its own loop thread.

/// Apply the loop settings to the calling thread.
///
/// Every call is best effort: a Windows build or a session that lacks the
/// privilege to raise its priority still runs correctly, just less predictably.
#[cfg(windows)]
pub(crate) fn tune_current_thread() {
    use windows_sys::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
        SetProcessInformation, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };

    // "Games", UTF-16, NUL-terminated.
    let mut task_name: Vec<u16> = "Games".encode_utf16().collect();
    task_name.push(0);

    // SAFETY: every call takes this process or thread and a pointer to a live,
    // correctly sized struct; no handle is stored past the call. All of them fail
    // harmlessly (return 0) when the platform refuses.
    unsafe {
        // EcoQoS / power throttling off for this process.
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
        SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let mut task_index = 0u32;
        AvSetMmThreadCharacteristicsW(task_name.as_ptr(), &mut task_index);
    }
}

/// No loop settings to apply outside Windows.
#[cfg(not(windows))]
pub(crate) fn tune_current_thread() {}
