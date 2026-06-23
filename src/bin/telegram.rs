//! Upbit notice → Telegram watcher.
//!
//! Polls the Upbit announcements LIST API, detects NEW notices, and pushes each
//! one to a Telegram chat via teloxide.
//!
//! The page https://www.upbit.com/service_center/notice is a React shell; the
//! list it renders comes from the api-manager JSON API. We poll that directly.
//! New-notice detection tracks the **maximum notice id** seen (not "row 0"),
//! because Upbit pins some notices to the top of the list — the newest real
//! notice isn't always first, but it always has the highest id.
//!
//! ⚠ Run where Upbit is reachable. upbit.com / api-manager.upbit.com are
//! geo-blocked + Cloudflare-protected from most regions — run this on the
//! Frankfurt server (or any allowed-region / VPN host).
//!
//! Env:
//!   TELOXIDE_TOKEN    (required) Telegram bot token
//!   UPBIT_CHAT_ID     target chat id (default 215555555)
//!   POLL_SECS         poll interval in seconds (default 60)
//!   UPBIT_SEEN_FILE   state file holding the last-seen max id (default ./upbit_seen_id.txt)
//!
//! First run with no state file records the current max id and sends NOTHING
//! (no backlog spam); only notices that appear afterwards are pushed. Delete
//! the state file (or write `0` into it) to treat everything as new.

use std::time::Duration;

use serde_json::Value;
use teloxide::prelude::*; // Bot, Requester (gives `.send_message`)
use teloxide::types::ChatId;

const LIST_URL: &str = "https://api-manager.upbit.com/api/v1/announcements\
?os=web&page=1&per_page=20&category=all&language=en";

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

const DEFAULT_CHAT_ID: i64 = 215_555_555;

struct Notice {
    id: i64,
    title: String,
}

#[tokio::main]
async fn main() {
    let token = std::env::var("TELOXIDE_TOKEN")
        .expect("TELOXIDE_TOKEN env var is required (the Telegram bot token)");
    let chat_id = ChatId(env_i64("UPBIT_CHAT_ID", DEFAULT_CHAT_ID));
    let poll_secs = env_u64("POLL_SECS", 60).max(5); // floor at 5s to avoid hammering
    let seen_file =
        std::env::var("UPBIT_SEEN_FILE").unwrap_or_else(|_| "upbit_seen_id.txt".to_string());

    let bot = Bot::new(token);
    let http = build_http();

    // Last-seen max id; None means "no baseline yet" (first run / wiped state).
    let mut seen_max: Option<i64> = read_seen(&seen_file);
    eprintln!(
        "[upbit-tg] start chat={} poll={}s seen_file={} seen_max={:?}",
        chat_id.0, poll_secs, seen_file, seen_max
    );

    // `interval` fires immediately on the first tick, so we poll right away.
    let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
    loop {
        tick.tick().await;

        let notices = match fetch_notices(&http).await {
            Ok(n) if n.is_empty() => {
                eprintln!("[upbit-tg] fetch ok but no notices parsed (response shape changed?)");
                continue;
            }
            Ok(n) => n,
            Err(e) => {
                eprintln!("[upbit-tg] fetch failed: {e} — retrying next poll");
                continue;
            }
        };

        let batch_max = notices.iter().map(|n| n.id).max().unwrap(); // non-empty checked above

        let prev = match seen_max {
            // First run: establish a baseline, push nothing.
            None => {
                seen_max = Some(batch_max);
                if let Err(e) = write_seen(&seen_file, batch_max) {
                    eprintln!("[upbit-tg] WARN: could not persist baseline: {e}");
                }
                eprintln!("[upbit-tg] baseline set to id={batch_max} (backlog not sent)");
                continue;
            }
            Some(p) => p,
        };

        // New = id strictly greater than the last sent. Send oldest-first so
        // the chat reads chronologically.
        let mut fresh: Vec<&Notice> = notices.iter().filter(|n| n.id > prev).collect();
        fresh.sort_by_key(|n| n.id);

        for n in fresh {
            let text = format!(
                "🔔 Upbit notice\n{}\n\nhttps://www.upbit.com/service_center/notice?id={}",
                n.title, n.id
            );
            match bot.send_message(chat_id, text).await {
                Ok(_) => {
                    // Advance + persist ONLY after a successful send, so a
                    // Telegram outage can never make us skip a notice.
                    seen_max = Some(n.id);
                    if let Err(e) = write_seen(&seen_file, n.id) {
                        eprintln!("[upbit-tg] WARN: sent id={} but persist failed: {e}", n.id);
                    }
                    eprintln!("[upbit-tg] sent id={} title={:?}", n.id, n.title);
                }
                Err(e) => {
                    // Don't advance past an unsent notice — retry from `prev`
                    // on the next poll. (At-least-once: a later success after a
                    // persist failure may re-send, which is safer than missing.)
                    eprintln!("[upbit-tg] telegram send failed id={}: {e} — retrying next poll", n.id);
                    break;
                }
            }
        }
    }
}

/// Async HTTP client with browser-like headers (helps with Cloudflare) and rustls.
fn build_http() -> reqwest::Client {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(UA));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/plain, */*"));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9,ko;q=0.5"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .build()
        .expect("build reqwest client")
}

/// Fetch + parse the Upbit announcements list into `Notice`s.
async fn fetch_notices(http: &reqwest::Client) -> Result<Vec<Notice>, String> {
    let resp = http.get(LIST_URL).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let snippet: String = body.chars().take(180).collect();
        return Err(format!("HTTP {status}: {snippet}"));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("json parse: {e}"))?;
    let arr = find_notices_array(&v)
        .ok_or_else(|| "no notices array in response".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let (Some(id), Some(title)) = (
            item.get("id").and_then(Value::as_i64),
            item.get("title").and_then(Value::as_str),
        ) {
            out.push(Notice { id, title: title.to_string() });
        }
    }
    Ok(out)
}

/// Locate the notices array. Prefer an array under a key literally named
/// "notices"; otherwise the first array whose elements look like notices
/// (have an integer `id` and a string `title`). Handles `data.notices[]`,
/// `data[]`, or other wrappers without a brittle struct.
fn find_notices_array(v: &Value) -> Option<&Vec<Value>> {
    find_array_by_key(v, "notices").or_else(|| find_notice_like_array(v))
}

fn find_array_by_key<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    match v {
        Value::Object(m) => {
            if let Some(Value::Array(a)) = m.get(key) {
                return Some(a);
            }
            m.values().find_map(|x| find_array_by_key(x, key))
        }
        Value::Array(a) => a.iter().find_map(|x| find_array_by_key(x, key)),
        _ => None,
    }
}

fn find_notice_like_array(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Array(a) => {
            if a.iter().any(is_notice_obj) {
                return Some(a);
            }
            a.iter().find_map(find_notice_like_array)
        }
        Value::Object(m) => m.values().find_map(find_notice_like_array),
        _ => None,
    }
}

fn is_notice_obj(v: &Value) -> bool {
    v.get("id").and_then(Value::as_i64).is_some()
        && v.get("title").and_then(Value::as_str).is_some()
}

/// Read the persisted last-seen max id (None if missing/unparseable).
fn read_seen(path: &str) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Persist the last-seen max id atomically (write tmp, then rename).
fn write_seen(path: &str, id: i64) -> std::io::Result<()> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, id.to_string())?;
    std::fs::rename(&tmp, path)
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(default)
}
