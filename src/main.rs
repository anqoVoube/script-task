//! Fetch an Upbit notice and report whether a phrase (default "Market Support")
//! appears in it — preferring the English rendering.
//!
//! The public page (www.upbit.com/service_center/notice?id=N) is an empty React
//! shell; the real title/body comes from the api-manager JSON API. That API
//! serves Korean by default. To get English we try, in order, the language
//! query-param variants Upbit's web client uses and an English `Accept-Language`
//! header, then pick whichever response actually comes back in English.
//!
//! Usage:
//!   upbit-notice                              # id 6303, "Market Support", English
//!   upbit-notice 6303 "Market Support"
//!   upbit-notice 6303 --lang ko               # force Korean
//!   upbit-notice 6303 --probe                 # try every variant, show a table
//!   upbit-notice 6303 --raw                   # also dump the chosen response body
//!   upbit-notice "<full-url>" "phrase"

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

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    En,
    Ko,
}

impl Lang {
    /// Accept-Language header value.
    fn accept(self) -> &'static str {
        match self {
            Lang::En => "en-US,en;q=0.9,ko;q=0.5",
            Lang::Ko => "ko-KR,ko;q=0.9,en;q=0.5",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ko => "ko",
        }
    }
}

struct Opts {
    target: String, // numeric id or a full URL
    is_url: bool,
    term: String,
    lang: Lang,
    raw: bool,
    probe: bool,
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "Target : {}\nPhrase : \"{}\" (case-insensitive)\nLang   : {}\n",
        opts.target,
        opts.term,
        opts.lang.label()
    );

    let client = match build_client(opts.lang) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };

    let urls = if opts.is_url {
        vec![opts.target.clone()]
    } else {
        candidates(&opts.target, opts.lang)
    };

    if opts.probe {
        return probe(&client, &urls, &opts.term);
    }

    // Search mode: fetch candidates in order, keep the first that succeeds, but
    // prefer one whose body actually reads as the requested language.
    let mut chosen: Option<(String, String)> = None; // (url, body)
    for url in &urls {
        println!("── GET {url}");
        match fetch(&client, url) {
            Ok(body) => {
                let detected = detect_lang(&body);
                println!("   detected language: {detected}");
                if looks_like_cloudflare_block(&body) {
                    println!("   ! Cloudflare challenge — IP may still be flagged");
                }
                let want = opts.lang.label();
                let is_match = detected == want;
                // Remember the first success; upgrade if a later one matches lang.
                let upgrade = match &chosen {
                    None => true,
                    Some((_, prev)) => is_match && detect_lang(prev) != want,
                };
                if upgrade {
                    chosen = Some((url.clone(), body));
                }
                if is_match {
                    break; // got the language we wanted — stop early
                }
            }
            Err(e) => println!("   ! fetch failed: {e}"),
        }
        println!();
    }

    let Some((url, body)) = chosen else {
        eprintln!("\nAll requests failed (geo-block / network). Run where Upbit is reachable.");
        return ExitCode::FAILURE;
    };

    println!("\n══ using: {url}");
    if let Some(title) = json_title(&body) {
        println!("   title: {title}");
    }
    let detected = detect_lang(&body);
    if detected != opts.lang.label() {
        println!(
            "   ! wanted {} but best response looks {detected} — \
             run with --probe to see all variants",
            opts.lang.label()
        );
    }
    if opts.raw {
        println!("\n----- raw body -----\n{body}\n--------------------\n");
    }

    let hits = find_ci_ascii(&body, &opts.term);
    println!("   {} byte(s), {} match(es)", body.len(), hits.len());
    for (n, &at) in hits.iter().enumerate() {
        println!("     [{}] …{}…", n + 1, context(&body, at, opts.term.len(), 70));
    }

    println!("\n══════════════════════════════════════════");
    if hits.is_empty() {
        println!("RESULT: \"{}\" NOT found.", opts.term);
        ExitCode::from(2)
    } else {
        println!("RESULT: \"{}\" FOUND — {} occurrence(s).", opts.term, hits.len());
        ExitCode::SUCCESS
    }
}

/// Try every candidate URL and print a compact diagnostic per variant so we can
/// empirically pick the one that returns English.
fn probe(client: &Client, urls: &[String], term: &str) -> ExitCode {
    println!("PROBE — {} variant(s)\n", urls.len());
    let mut any = false;
    for url in urls {
        print!("• {url}\n  ");
        match fetch_quiet(client, url) {
            Ok((status, body)) => {
                any = true;
                let lang = detect_lang(&body);
                let title = json_title(&body).unwrap_or_else(|| "<no json title>".into());
                let hits = find_ci_ascii(&body, term).len();
                println!(
                    "status={status} lang={lang} bytes={} \"{term}\"x{hits}\n  title: {}",
                    body.len(),
                    truncate(&title, 90)
                );
            }
            Err(e) => println!("FAILED: {e}"),
        }
        println!();
    }
    if any {
        println!("Pick the variant whose lang=en and title reads in English; tell me which\nquery-param worked and I'll make it the default.");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Candidate endpoints for a numeric id, English-first. Upbit's web client uses
/// a language query param; the exact name varies, so we try the common ones and
/// fall back to the bare endpoint (which English `Accept-Language` may still
/// flip).
fn candidates(id: &str, lang: Lang) -> Vec<String> {
    let base = format!("https://api-manager.upbit.com/api/v1/announcements/{id}");
    match lang {
        Lang::En => vec![
            format!("{base}?os=web&language=en"),
            format!("{base}?os=web&lang=en"),
            format!("{base}?os=web&locale=en"),
            format!("{base}?os=web"),
        ],
        Lang::Ko => vec![format!("{base}?os=web")],
    }
}

fn build_client(lang: Lang) -> reqwest::Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(UA));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/plain, */*"));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static(lang.accept()),
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
    println!("   status: {status}");
    resp.text().map_err(|e| e.to_string())
}

fn fetch_quiet(client: &Client, url: &str) -> Result<(reqwest::StatusCode, String), String> {
    let resp = client.get(url).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().map_err(|e| e.to_string())?;
    Ok((status, body))
}

/// Heuristic language label: any Hangul syllables => "ko", else "en".
fn detect_lang(s: &str) -> &'static str {
    if s.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)) {
        "ko"
    } else {
        "en"
    }
}

/// Case-insensitive (ASCII) substring search → byte offsets into the original
/// UTF-8 string (so offsets stay valid for slicing a mixed Korean/ASCII body).
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

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
        || (b.contains("cloudflare") && b.contains("blocked"))
}

fn parse_args() -> Result<Opts, String> {
    let mut positionals: Vec<String> = Vec::new();
    let mut lang = Lang::En;
    let mut raw = false;
    let mut probe = false;

    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--raw" => raw = true,
            "--probe" => probe = true,
            "-h" | "--help" => {
                println!("usage: upbit-notice [id|url] [term] [--lang en|ko] [--probe] [--raw]");
                std::process::exit(0);
            }
            "--lang" | "-l" => {
                let v = it.next().ok_or("--lang needs a value (en|ko)")?;
                lang = parse_lang(&v)?;
            }
            s if s.starts_with("--lang=") => lang = parse_lang(&s[7..])?,
            s if s.starts_with("--") => return Err(format!("unknown flag {s}")),
            other => positionals.push(other.to_string()),
        }
    }

    let first = positionals.first().cloned();
    let (target, is_url) = match first {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => (u, true),
        Some(id) => (id, false),
        None => (DEFAULT_ID.to_string(), false),
    };
    let term = positionals.get(1).cloned().unwrap_or_else(|| DEFAULT_TERM.to_string());

    Ok(Opts { target, is_url, term, lang, raw, probe })
}

fn parse_lang(s: &str) -> Result<Lang, String> {
    match s.to_ascii_lowercase().as_str() {
        "en" | "english" => Ok(Lang::En),
        "ko" | "kr" | "korean" => Ok(Lang::Ko),
        other => Err(format!("unknown lang '{other}' (use en|ko)")),
    }
}
