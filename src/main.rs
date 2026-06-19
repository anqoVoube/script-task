//! Fetch an Upbit notice and report whether a phrase (default "Market Support")
//! appears in it.
//!
//! The public notice page (www.upbit.com/service_center/notice?id=N) is an empty
//! React shell: the real title/body is loaded over XHR from the api-manager JSON
//! API. So by default we hit the JSON API first and fall back to the share page.
//!
//! Usage:
//!   upbit-notice                      # id 6303, term "Market Support"
//!   upbit-notice 6303                 # explicit id
//!   upbit-notice 6303 "Market Support"
//!   upbit-notice "https://www.upbit.com/service_center/notice?id=6303&view=share"
//!   upbit-notice "<full-url>" "some phrase"

use std::process::ExitCode;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, ORIGIN, REFERER, USER_AGENT,
};

const DEFAULT_ID: &str = "6303";
const DEFAULT_TERM: &str = "Market Support";

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let second = args.next();

    // Resolve target id-or-url and the search term.
    let (targets, label) = match first.as_deref() {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
            (vec![u.to_string()], u.to_string())
        }
        Some(id) => (targets_for_id(id), format!("id={id}")),
        None => (targets_for_id(DEFAULT_ID), format!("id={DEFAULT_ID}")),
    };
    let term = second.unwrap_or_else(|| DEFAULT_TERM.to_string());

    println!("Target : {label}");
    println!("Phrase : \"{term}\" (case-insensitive)\n");

    let client = match build_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut total_matches = 0usize;
    let mut any_success = false;

    for url in &targets {
        println!("── GET {url}");
        match fetch(&client, url) {
            Ok(body) => {
                any_success = true;
                if looks_like_cloudflare_block(&body) {
                    println!(
                        "   ! response looks like a Cloudflare challenge/block \
                         (your IP may still be flagged — use the VPN/server you intended)\n"
                    );
                }
                if let Some(title) = json_title(&body) {
                    println!("   title: {title}");
                }
                let hits = find_ci_ascii(&body, &term);
                println!("   {} byte(s) received, {} match(es)", body.len(), hits.len());
                for (n, &at) in hits.iter().enumerate() {
                    println!("     [{}] …{}…", n + 1, context(&body, at, term.len(), 70));
                }
                total_matches += hits.len();
                println!();
            }
            Err(e) => {
                println!("   ! fetch failed: {e}\n");
            }
        }
    }

    if !any_success {
        eprintln!("All requests failed (geo-block / network). Run this where the URL is reachable.");
        return ExitCode::FAILURE;
    }

    println!("══════════════════════════════════════════");
    if total_matches > 0 {
        println!("RESULT: \"{term}\" FOUND — {total_matches} occurrence(s).");
        ExitCode::SUCCESS
    } else {
        println!("RESULT: \"{term}\" NOT found.");
        // Exit 2 => reachable but no match, distinct from network failure (1).
        ExitCode::from(2)
    }
}

/// Build the candidate URLs for a numeric notice id: JSON API first, then the
/// human share page as a fallback.
fn targets_for_id(id: &str) -> Vec<String> {
    vec![
        format!("https://api-manager.upbit.com/api/v1/announcements/{id}?os=web"),
        format!("https://www.upbit.com/service_center/notice?id={id}&view=share"),
    ]
}

fn build_client() -> reqwest::Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(UA));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("ko-KR,ko;q=0.9,en-US;q=0.8,en;q=0.7"),
    );
    headers.insert(ORIGIN, HeaderValue::from_static("https://www.upbit.com"));
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://www.upbit.com/service_center/notice"),
    );
    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
}

fn fetch(client: &Client, url: &str) -> Result<String, String> {
    let resp = client.get(url).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().map_err(|e| e.to_string())?;
    println!("   status: {status}");
    Ok(body)
}

/// Case-insensitive (ASCII) substring search returning byte offsets into the
/// original UTF-8 string, so offsets stay valid for context slicing even when
/// the body mixes Korean and ASCII.
fn find_ci_ascii(haystack: &str, needle: &str) -> Vec<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut out = Vec::new();
    if n.is_empty() || h.len() < n.len() {
        return out;
    }
    'outer: for i in 0..=h.len() - n.len() {
        for (j, &nb) in n.iter().enumerate() {
            if !h[i + j].eq_ignore_ascii_case(&nb) {
                continue 'outer;
            }
        }
        out.push(i);
    }
    out
}

/// Return a `pad`-byte window around a match, snapped to char boundaries and
/// with newlines flattened to keep one-line output.
fn context(s: &str, at: usize, len: usize, pad: usize) -> String {
    let mut start = at.saturating_sub(pad);
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + len + pad).min(s.len());
    while end < s.len() && !s.is_char_boundary(end) {
        end += 1;
    }
    s[start..end].replace(['\n', '\r'], " ").trim().to_string()
}

/// If the body is JSON, walk it for the first "title" string value.
fn json_title(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    find_title(&v)
}

fn find_title(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(t)) = map.get("title") {
                return Some(t.clone());
            }
            map.values().find_map(find_title)
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(find_title),
        _ => None,
    }
}

fn looks_like_cloudflare_block(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("attention required")
        || b.contains("cf-error-details")
        || b.contains("cloudflare") && b.contains("blocked")
}
