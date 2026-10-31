//! `hands` - Windows input injection / hook spike.
//!
//! All output is JSON lines on `--log <path>` plus a short human summary on
//! stdout. Injecting subcommands are gated by the foreground guard, the
//! physical-input abort hook, the watchdog and `--max-secs`.

mod log;
mod percentiles;
mod safety;
mod scancode;
mod timer_mode;
mod watchdog;

use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use safety::{release_all, Slicer, WaitOutcome};

pub fn now_ns_pub() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn now_ns() -> u64 {
    now_ns_pub()
}

fn usage() {
    eprintln!(
        "usage: hands <subcommand> [options]\n\
         \n\
         subcommands:\n\
           mouse-move --counts DX DY --repeat N --interval-ms M\n\
           key-hold --scancode 0x11 --hold-ms H --repeat N --gap-ms G\n\
           hook-log --secs S\n\
           timer-jitter --period-ms 50 --secs S\n\
           takeover-test --scancode 0x11 --secs S\n\
           watchdog --pid PID\n\
         \n\
         global options:\n\
           --log PATH          JSON-lines output file\n\
           --max-secs N        hard limit from program start (default 60, 0 = off)\n\
           --delay-secs N      countdown before injecting (default 5)\n\
           --title-prefix S    required foreground window prefix (default Minecraft)\n\
           --jitter-px N       mouse jump abort threshold (default 3)\n\
           --drift-px N        total mouse drift abort threshold (default 20)\n\
           --no-watchdog       do not spawn the watchdog child (on by default for injecting subcommands)\n\
         \n\
         environment:\n\
           HANDS_TIMER_MODE    comma list: nothrottle, high, mmcss (applied to every subcommand)\n\
         \n\
         exit codes: 0 ok, 1 max-secs, 2 usage, 3 foreground lost, 4 physical abort, 5 hook install failed, 6 SendInput failed"
    );
}

fn value<'a>(rest: &'a [String], i: usize, name: &str) -> Result<&'a str, String> {
    rest.get(i + 1)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("missing value for {}", name))
}

fn parse_u64(s: &str, name: &str) -> Result<u64, String> {
    s.parse::<u64>()
        .map_err(|_| format!("bad value for {}: {}", name, s))
}

fn parse_i32(s: &str, name: &str) -> Result<i32, String> {
    s.parse::<i32>()
        .map_err(|_| format!("bad value for {}: {}", name, s))
}

fn or_exit<T>(r: Result<T, String>) -> T {
    r.unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        exit(2);
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 {
        usage();
        exit(2);
    }
    if argv[1] == "--help" || argv[1] == "-h" {
        usage();
        exit(0);
    }
    // D5: the --max-secs deadline counts from program start, including the
    // countdown, so it is taken before anything else happens.
    let program_start = now_ns_pub();
    let sub = argv[1].clone();
    let mut rest: Vec<String> = argv[2..].to_vec();

    let mut log_path: Option<String> = None;
    let mut max_secs: u64 = 60;
    let mut delay_secs: u64 = 5;
    let mut title_prefix: String = "Minecraft".to_string();
    let mut jitter_px: i32 = 3;
    let mut drift_px: i32 = 20;
    // The watchdog is on by default for injecting subcommands; `--no-watchdog` turns it off.
    let mut watchdog_flag = true;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--log" => {
                log_path = Some(or_exit(value(&rest, i, "--log").map(str::to_string)));
                rest.drain(i..=i + 1);
                continue;
            }
            "--max-secs" => {
                max_secs = or_exit(parse_u64(
                    or_exit(value(&rest, i, "--max-secs")),
                    "--max-secs",
                ));
                rest.drain(i..=i + 1);
                continue;
            }
            "--delay-secs" => {
                delay_secs = or_exit(parse_u64(
                    or_exit(value(&rest, i, "--delay-secs")),
                    "--delay-secs",
                ));
                rest.drain(i..=i + 1);
                continue;
            }
            "--title-prefix" => {
                title_prefix = or_exit(value(&rest, i, "--title-prefix").map(str::to_string));
                rest.drain(i..=i + 1);
                continue;
            }
            "--jitter-px" => {
                jitter_px = or_exit(parse_i32(
                    or_exit(value(&rest, i, "--jitter-px")),
                    "--jitter-px",
                ));
                rest.drain(i..=i + 1);
                continue;
            }
            "--drift-px" => {
                drift_px = or_exit(parse_i32(
                    or_exit(value(&rest, i, "--drift-px")),
                    "--drift-px",
                ));
                rest.drain(i..=i + 1);
                continue;
            }
            "--watchdog" => {
                watchdog_flag = true;
                rest.remove(i);
                continue;
            }
            "--no-watchdog" => {
                watchdog_flag = false;
                rest.remove(i);
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    if let Some(p) = &log_path {
        if let Err(e) = log::init(p) {
            eprintln!("log init error: {}", e);
            exit(2);
        }
    }

    safety::init(jitter_px, drift_px);

    // D6: deliberately kept. HANDS_TIMER_MODE is applied to every subcommand,
    // not only timer-jitter.
    let mode = std::env::var("HANDS_TIMER_MODE").unwrap_or_default();
    timer_mode::apply(&mode);
    // D2: lifting the timer resolution is needed by every injecting command.
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    }

    let injecting = matches!(sub.as_str(), "mouse-move" | "key-hold" | "takeover-test");
    if watchdog_flag && injecting {
        if let Err(e) = safety::spawn_watchdog() {
            // Do not inject when the requested safety net is missing.
            eprintln!("watchdog: {}", e);
            exit(2);
        }
    }

    let res = match sub.as_str() {
        "mouse-move" => cmd_mouse_move(&rest, &title_prefix, program_start, max_secs, delay_secs),
        "key-hold" => cmd_key_hold(&rest, &title_prefix, program_start, max_secs, delay_secs),
        "hook-log" => cmd_hook_log(&rest, max_secs),
        "timer-jitter" => cmd_timer_jitter(&rest, max_secs),
        "takeover-test" => {
            cmd_takeover_test(&rest, &title_prefix, program_start, max_secs, delay_secs)
        }
        "watchdog" => cmd_watchdog(&rest),
        _ => {
            eprintln!("unknown subcommand: {}", sub);
            exit(2);
        }
    };

    release_all();
    safety::stop_hooks();
    unsafe {
        windows_sys::Win32::Media::timeEndPeriod(1);
    }

    match res {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("error: {}", e);
            exit(2);
        }
    }
}

fn countdown(delay_secs: u64, program_start: u64, max_secs: u64) {
    for s in (1..=delay_secs).rev() {
        // D5: the hard limit also covers the countdown, so stop waiting once
        // the budget is gone (nothing has been injected yet).
        if max_secs > 0 && now_ns().saturating_sub(program_start) >= max_secs * 1_000_000_000
        {
            return;
        }
        println!("{}", s);
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn max_secs_exceeded(program_start: u64, max_secs: u64) -> bool {
    max_secs > 0 && now_ns().saturating_sub(program_start) >= max_secs * 1_000_000_000
}

fn foreground_or_exit(prefix: &str) {
    if !safety::foreground_ok(prefix) {
        release_all();
        exit(3);
    }
}

/// M3/D3: one abort record with every stamp the brief asks for.
fn abort_exit() -> ! {
    let hook_t = safety::first_physical_time();
    let flag_seen_t = now_ns();
    let release_send_t = now_ns();
    release_all();
    let release_done_t = now_ns();
    let latency_us = if hook_t > 0 && release_send_t > hook_t {
        (release_send_t - hook_t) / 1000
    } else {
        0
    };
    log::line(&format!(
        "{{\"t\":{},\"event\":\"abort\",\"hook_t\":{},\"flag_seen_t\":{},\"release_send_t\":{},\"release_done_t\":{},\"latency_us\":{},\"event_id\":{}}}",
        release_done_t,
        hook_t,
        flag_seen_t,
        release_send_t,
        release_done_t,
        latency_us,
        safety::first_physical_event()
    ));
    println!("abort latency_us={}", latency_us);
    exit(4);
}

fn finish_wait(outcome: WaitOutcome) -> i32 {
    match outcome {
        WaitOutcome::Done => 0,
        WaitOutcome::Aborted => abort_exit(),
        WaitOutcome::ForegroundLost => {
            release_all();
            3
        }
        WaitOutcome::Timeout => {
            release_all();
            1
        }
        WaitOutcome::HookDead => {
            log::line(&format!(
                "{{\"t\":{},\"event\":\"hook_heartbeat_lost\"}}",
                now_ns()
            ));
            release_all();
            4
        }
    }
}

fn effective_duration(secs: u64, max_secs: u64) -> u64 {
    if max_secs == 0 {
        secs
    } else {
        secs.min(max_secs)
    }
}

fn cmd_mouse_move(
    rest: &[String],
    prefix: &str,
    program_start: u64,
    max_secs: u64,
    delay_secs: u64,
) -> Result<i32, String> {
    let mut dx: i32 = 0;
    let mut dy: i32 = 0;
    let mut repeat: u64 = 1;
    let mut interval_ms: u64 = 16;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--counts" => {
                dx = parse_i32(value(rest, i, "--counts")?, "--counts")?;
                dy = parse_i32(value(rest, i + 1, "--counts")?, "--counts")?;
                i += 3;
                continue;
            }
            "--repeat" => {
                repeat = parse_u64(value(rest, i, "--repeat")?, "--repeat")?;
                i += 2;
                continue;
            }
            "--interval-ms" => {
                interval_ms = parse_u64(value(rest, i, "--interval-ms")?, "--interval-ms")?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    let mut speed: i32 = 0;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows_sys::Win32::UI::WindowsAndMessaging::SPI_GETMOUSESPEED,
            0,
            &mut speed as *mut _ as *mut _,
            0,
        );
    }
    let mut mouse_params: [i32; 3] = [0; 3];
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows_sys::Win32::UI::WindowsAndMessaging::SPI_GETMOUSE,
            0,
            mouse_params.as_mut_ptr() as *mut _,
            0,
        );
    }
    log::line(&format!(
        "{{\"t\":{},\"event\":\"start\",\"spi_mouse_speed\":{},\"spi_mouse\":[{},{},{}],\"enhanced_precision\":{}}}",
        now_ns(),
        speed,
        mouse_params[0],
        mouse_params[1],
        mouse_params[2],
        mouse_params[2]
    ));

    countdown(delay_secs, program_start, max_secs);
    if max_secs_exceeded(program_start, max_secs) {
        return Ok(1);
    }
    foreground_or_exit(prefix);
    safety::start_hooks();

    let slicer = Slicer::new();
    let base = now_ns();
    let interval_ns = interval_ms * 1_000_000;
    for n in 0..repeat {
        if safety::abort() {
            abort_exit();
        }
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        if max_secs > 0 && now_ns().saturating_sub(program_start) >= max_secs * 1_000_000_000 {
            release_all();
            return Ok(1);
        }

        let send_t = now_ns();
        if safety::send_mouse_move(dx, dy).is_none() {
            return Ok(if safety::abort() {
                abort_exit()
            } else {
                release_all();
                4
            });
        }
        let done_t = now_ns();
        // S7: re-check right after the send.
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        log::line(&format!(
            "{{\"t\":{},\"event\":\"send\",\"n\":{},\"dx\":{},\"dy\":{},\"send_t\":{},\"done_t\":{}}}",
            send_t, n, dx, dy, send_t, done_t
        ));

        // D1: absolute deadline from a fixed base, so no cumulative drift.
        let deadline = base + (n + 1) * interval_ns;
        let outcome = slicer.wait(deadline, program_start, max_secs, prefix);
        if outcome != WaitOutcome::Done {
            return Ok(finish_wait(outcome));
        }
    }
    Ok(0)
}

fn cmd_key_hold(
    rest: &[String],
    prefix: &str,
    program_start: u64,
    max_secs: u64,
    delay_secs: u64,
) -> Result<i32, String> {
    let mut scancode_str = "0x11".to_string();
    let mut hold_ms: u64 = 50;
    let mut repeat: u64 = 1;
    let mut gap_ms: u64 = 50;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--scancode" => {
                scancode_str = value(rest, i, "--scancode")?.to_string();
                i += 2;
                continue;
            }
            "--hold-ms" => {
                hold_ms = parse_u64(value(rest, i, "--hold-ms")?, "--hold-ms")?;
                i += 2;
                continue;
            }
            "--repeat" => {
                repeat = parse_u64(value(rest, i, "--repeat")?, "--repeat")?;
                i += 2;
                continue;
            }
            "--gap-ms" => {
                gap_ms = parse_u64(value(rest, i, "--gap-ms")?, "--gap-ms")?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let (scan, ext) = scancode::parse(&scancode_str)?;

    countdown(delay_secs, program_start, max_secs);
    if max_secs_exceeded(program_start, max_secs) {
        return Ok(1);
    }
    foreground_or_exit(prefix);
    safety::start_hooks();

    let slicer = Slicer::new();
    let base = now_ns();
    let hold_ns = hold_ms * 1_000_000;
    let gap_ns = gap_ms * 1_000_000;
    for n in 0..repeat {
        let cycle_start = base + n * (hold_ns + gap_ns);
        if safety::abort() {
            abort_exit();
        }
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        if max_secs > 0 && now_ns().saturating_sub(program_start) >= max_secs * 1_000_000_000 {
            release_all();
            return Ok(1);
        }

        let down_send_t = now_ns();
        if safety::send_key(scan, ext, true).is_none() {
            release_all();
            return Ok(if safety::abort() {
                abort_exit()
            } else {
                4
            });
        }
        let down_done_t = now_ns();
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        log::line(&format!(
            "{{\"t\":{},\"event\":\"down\",\"n\":{},\"scancode\":\"{}\",\"down_send_t\":{},\"down_done_t\":{}}}",
            down_send_t, n, scancode_str, down_send_t, down_done_t
        ));

        // S3/D5: hold is sliced, so abort/foreground/max-secs are seen while
        // the key is down.
        let hold_deadline = cycle_start + hold_ns;
        let outcome = slicer.wait(hold_deadline, program_start, max_secs, prefix);
        if outcome != WaitOutcome::Done {
            return Ok(finish_wait(outcome));
        }

        let up_send_t = now_ns();
        let up_done_t = safety::send_key(scan, ext, false).unwrap_or_else(now_ns);
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        log::line(&format!(
            "{{\"t\":{},\"event\":\"up\",\"n\":{},\"scancode\":\"{}\",\"up_send_t\":{},\"up_done_t\":{}}}",
            up_send_t, n, scancode_str, up_send_t, up_done_t
        ));

        let gap_deadline = cycle_start + hold_ns + gap_ns;
        let outcome = slicer.wait(gap_deadline, program_start, max_secs, prefix);
        if outcome != WaitOutcome::Done {
            return Ok(finish_wait(outcome));
        }
    }
    Ok(0)
}

fn cmd_hook_log(rest: &[String], max_secs: u64) -> Result<i32, String> {
    let mut secs: u64 = 5;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--secs" => {
                secs = parse_u64(value(rest, i, "--secs")?, "--secs")?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let duration = effective_duration(secs, max_secs);
    safety::start_hooks_log();
    let start = now_ns();
    let end = start + duration * 1_000_000_000;
    while now_ns() < end {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    safety::stop_hooks();
    let elapsed = now_ns().saturating_sub(start);
    let s = safety::hook_log_summary(elapsed);
    println!(
        "windows={} nonempty={} p50={:.2} p99={:.2} max={:.2} (per 100ms movement magnitude)",
        s.windows_total, s.windows_nonempty, s.p50, s.p99, s.max
    );
    log::line(&format!(
        "{{\"t\":{},\"event\":\"summary\",\"windows\":{},\"nonempty\":{},\"p50\":{},\"p99\":{},\"max\":{}}}",
        now_ns(),
        s.windows_total,
        s.windows_nonempty,
        s.p50,
        s.p99,
        s.max
    ));
    Ok(0)
}

fn cmd_timer_jitter(rest: &[String], max_secs: u64) -> Result<i32, String> {
    let mut period_ms: u64 = 50;
    let mut secs: u64 = 5;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--period-ms" => {
                period_ms = parse_u64(value(rest, i, "--period-ms")?, "--period-ms")?;
                i += 2;
                continue;
            }
            "--secs" => {
                secs = parse_u64(value(rest, i, "--secs")?, "--secs")?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let duration = effective_duration(secs, max_secs);
    if period_ms == 0 {
        period_ms = 1;
    }
    let count = (duration * 1000) / period_ms;
    let period_ns = period_ms * 1_000_000;

    let timer = safety::create_waitable_timer_highres();
    let method = if timer != 0 {
        "high-res-waitable-timer"
    } else {
        "timeBeginPeriod+sleep"
    };

    let mut latencies: Vec<i64> = Vec::with_capacity(count as usize);
    let start = now_ns();
    for k in 1..=count {
        let deadline = start + k * period_ns;
        if timer != 0 {
            safety::wait_until_highres(timer, deadline);
        } else {
            // M8: bounded sleeps, never an unbounded spin.
            while now_ns() < deadline {
                let rem = deadline - now_ns();
                if rem > 2_000_000 {
                    std::thread::sleep(std::time::Duration::from_millis(
                        (rem / 1_000_000).saturating_sub(1).max(1),
                    ));
                } else {
                    std::thread::sleep(std::time::Duration::from_nanos(rem));
                }
            }
        }
        let wake = now_ns();
        let late_us = (wake as i64 - deadline as i64) / 1000;
        latencies.push(late_us);
        log::line(&format!(
            "{{\"t\":{},\"event\":\"wake\",\"deadline_t\":{},\"lateness_us\":{}}}",
            wake, deadline, late_us
        ));
    }
    safety::close_handle(timer);

    latencies.sort();
    let p50 = percentiles::percentile_i64(&latencies, 0.50);
    let p99 = percentiles::percentile_i64(&latencies, 0.99);
    let p999 = percentiles::percentile_i64(&latencies, 0.999);
    let mx = percentiles::percentile_i64(&latencies, 1.0);
    println!(
        "method={} n={} p50_us={} p99_us={} p99.9_us={} max_us={}",
        method,
        latencies.len(),
        p50,
        p99,
        p999,
        mx
    );
    log::line(&format!(
        "{{\"t\":{},\"event\":\"summary\",\"method\":\"{}\",\"n\":{},\"p50_us\":{},\"p99_us\":{},\"p99.9_us\":{},\"max_us\":{}}}",
        now_ns(),
        method,
        latencies.len(),
        p50,
        p99,
        p999,
        mx
    ));
    Ok(0)
}

fn cmd_takeover_test(
    rest: &[String],
    prefix: &str,
    program_start: u64,
    max_secs: u64,
    delay_secs: u64,
) -> Result<i32, String> {
    let mut scancode_str = "0x11".to_string();
    let mut secs: u64 = 5;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--scancode" => {
                scancode_str = value(rest, i, "--scancode")?.to_string();
                i += 2;
                continue;
            }
            "--secs" => {
                secs = parse_u64(value(rest, i, "--secs")?, "--secs")?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let (scan, ext) = scancode::parse(&scancode_str)?;
    let duration = effective_duration(secs, max_secs);

    countdown(delay_secs, program_start, max_secs);
    if max_secs_exceeded(program_start, max_secs) {
        return Ok(1);
    }
    foreground_or_exit(prefix);
    safety::start_hooks();

    let _ = safety::send_key(scan, ext, true);
    log::line(&format!(
        "{{\"t\":{},\"event\":\"hold\",\"scancode\":\"{}\",\"hook_t\":{}}}",
        now_ns(),
        scancode_str,
        safety::first_physical_time()
    ));

    let start = now_ns();
    let end = start + duration * 1_000_000_000;
    let hard_end = if max_secs == 0 {
        u64::MAX
    } else {
        program_start + max_secs * 1_000_000_000
    };
    let loop_end = end.min(hard_end);
    let mut lease_at = start + 50_000_000;
    let mut triggered = false;
    while now_ns() < loop_end {
        if safety::abort() {
            triggered = true;
            break;
        }
        if !safety::foreground_ok(prefix) {
            release_all();
            return Ok(3);
        }
        // D4: lease renewal every 50 ms; M1: abort polled every 1 ms.
        if now_ns() >= lease_at {
            let _ = safety::send_key(scan, ext, true);
            // S7: re-check right after this SendInput too.
            if !safety::foreground_ok(prefix) {
                release_all();
                return Ok(3);
            }
            log::line(&format!(
                "{{\"t\":{},\"event\":\"lease_renew\",\"scancode\":\"{}\"}}",
                now_ns(),
                scancode_str
            ));
            lease_at += 50_000_000;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let hook_t = safety::first_physical_time();
    let flag_seen_t = now_ns();
    let release_send_t = now_ns();
    // S2: bypass the abort gate; the logged release must really be sent.
    safety::release_key_now(scan, ext);
    let release_done_t = now_ns();
    log::line(&format!(
        "{{\"t\":{},\"event\":\"release\",\"triggered\":{},\"hook_t\":{},\"flag_seen_t\":{},\"release_send_t\":{},\"release_done_t\":{},\"event_id\":{}}}",
        release_done_t,
        triggered,
        hook_t,
        flag_seen_t,
        release_send_t,
        release_done_t,
        safety::first_physical_event()
    ));

    if triggered {
        let latency_us = if hook_t > 0 && release_send_t > hook_t {
            (release_send_t - hook_t) / 1000
        } else {
            0
        };
        println!("triggered latency_us={}", latency_us);
        log::line(&format!(
            "{{\"t\":{},\"event\":\"latency\",\"hook_event_t\":{},\"flag_seen_t\":{},\"release_send_t\":{},\"release_done_t\":{},\"latency_us\":{}}}",
            now_ns(),
            hook_t,
            flag_seen_t,
            release_send_t,
            release_done_t,
            latency_us
        ));
    } else {
        println!("not triggered");
    }
    Ok(0)
}

fn cmd_watchdog(rest: &[String]) -> Result<i32, String> {
    let mut pid: u32 = 0;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--pid" => {
                pid = value(rest, i, "--pid")?
                    .parse()
                    .map_err(|_| "bad value for --pid".to_string())?;
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    watchdog::run(pid)
}
