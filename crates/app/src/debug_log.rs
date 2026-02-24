use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_COUNTER: AtomicU64 = AtomicU64::new(1);
const DEBUG_LOG_PATH: &str = "/Users/irashid/personal/wizard-editor/.cursor/debug.log";

fn escape_json(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

pub fn emit(hypothesis_id: &str, location: &str, message: &str, data: &str) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let id = format!(
        "log_{}_{}",
        timestamp,
        LOG_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let payload = format!(
        "{{\"id\":\"{}\",\"timestamp\":{},\"location\":\"{}\",\"message\":\"{}\",\"data\":{{\"raw\":\"{}\"}},\"runId\":\"initial\",\"hypothesisId\":\"{}\"}}",
        escape_json(&id),
        timestamp,
        escape_json(location),
        escape_json(message),
        escape_json(data),
        escape_json(hypothesis_id)
    );

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(DEBUG_LOG_PATH)
    {
        let _ = writeln!(file, "{payload}");
    }
}
