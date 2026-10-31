//! JSON-lines log sink. All calls come from ordinary threads, never from a hook
//! procedure (S5): hook events go through the lock-free ring in `safety` and are
//! formatted here by the hook logger thread.

use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static LOG: Mutex<Option<File>> = Mutex::new(None);
static LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init(path: &str) -> Result<(), String> {
    let f = File::create(path).map_err(|e| e.to_string())?;
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    LOGGING_ENABLED.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn line(s: &str) {
    if !LOGGING_ENABLED.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{}", s);
            let _ = f.flush();
        }
    }
}
