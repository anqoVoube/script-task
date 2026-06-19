//! Fetch an Upbit notice and print its title — in English by default.
//!
//! The public page (www.upbit.com/service_center/notice?id=N) is an empty React
//! shell; the real title/body comes from the api-manager JSON API. That API
//! serves Korean by default; `?os=web&language=en` returns English.
//!
//! The title is printed to STDOUT (clean, one line, script-friendly). All
//! diagnostics — the URL, status, category, and any --match results — go to
//! STDERR, so `upbit-notice 6303` pipes the bare title.
//!
//! Usage:
//!   upbit-notice                       # id 6303, English title
//!   upbit-notice 6303                  # explicit id
//!   upbit-notice 6303 --lang ko        # Korean title
//!   upbit-notice 6303 --body           # also print the body text
//!   upbit-notice 6303 --match "Market Support"   # also report phrase hits (stderr)
//!   upbit-notice 6303 --probe          # try every endpoint variant
//!   upbit-notice "<full-url>"

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
    lang: Lang,
    raw: bool,
    body: bool,
    probe: bool,
    match_term: Option<String>, // optional phrase to additionally search for
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Diagnostics go to stderr so stdout carries only the title (script-friendly).
    eprintln!("[target {} | lang {}]", opts.target, opts.lang.label());

    let client = match build_client(opts.lang) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build HTTP client: {e}");
            return ExitCode::FAILURE;
        }
    };

    if opts.probe {
        let term = opts.match_term.clone().unwrap_or_else(|| DEFAULT_TERM.to_string());
        let urls = if opts.is_url {
            vec![opts.target.clone()]
        } else {
            probe_candidates(&opts.target)
        };
        return probe(&client, &urls, &term);
    }

    let urls = if opts.is_url {
        vec![opts.target.clone()]
    } else {
        candidates(&opts.target, opts.lang)
    };

    // Fetch the first candidate that succeeds.
    let mut chosen: Option<(String, String)> = None; // (url, raw body)
    for url in &urls {
        eprintln!("── GET {url}");
        match fetch(&client, url) {
            Ok(raw) => {
                if looks_like_cloudflare_block(&raw) {
                    eprintln!("   ! Cloudflare challenge — IP may still be flagged");
                }
                chosen = Some((url.clone(), raw));
                break;
            }
            Err(e) => eprintln!("   ! fetch failed: {e}"),
        }
    }

    let Some((_url, raw)) = chosen else {
        eprintln!("All requests failed (geo-block / network). Run where Upbit is reachable.");
        return ExitCode::FAILURE;
    };

    let title = json_field(&raw, "title");
    let category = json_field(&raw, "category");

    // The title is the product: print it to stdout, plainly.
    match &title {
        Some(t) => println!("{t}"),
        None => {
            eprintln!("error: no 'title' field in response (run with --raw to inspect)");
            if opts.raw {
                eprintln!("----- raw JSON -----\n{raw}\n--------------------");
            }
            return ExitCode::from(2);
        }
    }
    if let Some(c) = &category {
        eprintln!("[category {c}]");
    }

    if opts.body {
        let body_text = html_to_text(&json_field(&raw, "body").unwrap_or_default());
        if !body_text.trim().is_empty() {
            println!("\n{}", body_text.trim());
        }
    }
    if opts.raw {
        eprintln!("----- raw JSON -----\n{raw}\n--------------------");
    }

    // Optional, opt-in only: search for a phrase when --match is given.
    if let Some(term) = &opts.match_term {
        let hits = find_ci_ascii(&raw, term);
        eprintln!("[\"{}\" — {} match(es)]", term, hits.len());
        for (n, &at) in hits.iter().enumerate() {
            eprintln!("  [{}] …{}…", n + 1, context(&raw, at, term.len(), 70));
        }
    }

    ExitCode::SUCCESS
}

/// Try every candidate URL and print a compact diagnostic per variant so we can
/// empirically pick the one that returns English.
fn probe(client: &Client, urls: &[String], term: &str) -> ExitCode {
    println!("PROBE — {} variant(s)\n", urls.len());
    let mut any = false;
    for url in urls {
        print!("• {url}\n  ");
        match fetch_quiet(client, url) {
            Ok((status, raw)) => {
                any = true;
                let body_lang = detect_lang(&html_to_text(&json_field(&raw, "body").unwrap_or_default()));
                let title = json_field(&raw, "title").unwrap_or_else(|| "<no title>".into());
                let category = json_field(&raw, "category").unwrap_or_else(|| "?".into());
                let hits = find_ci_ascii(&raw, term).len();
                println!(
                    "status={status} body_lang={body_lang} category={category} \"{term}\"x{hits}\n  title: {}",
                    truncate(&title, 90)
                );
            }
            Err(e) => println!("FAILED: {e}"),
        }
        println!();
    }
    if any {
        println!("The variant with body_lang=en is the English one.");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Endpoint for a numeric id. Confirmed: `?os=web&language=en` returns the
/// English title/category/body (e.g. title "Market Support for Re(RE) …",
/// category "Trade"); `language=ko` (or the bare endpoint) returns Korean.
fn candidates(id: &str, lang: Lang) -> Vec<String> {
    let base = format!("https://api-manager.upbit.com/api/v1/announcements/{id}");
    vec![format!("{base}?os=web&language={}", lang.label())]
}

/// The full set of variants to try in `--probe` mode, so we can re-verify the
/// language switch on any notice.
fn probe_candidates(id: &str) -> Vec<String> {
    let base = format!("https://api-manager.upbit.com/api/v1/announcements/{id}");
    vec![
        format!("{base}?os=web&language=en"),
        format!("{base}?os=web&lang=en"),
        format!("{base}?os=web&locale=en"),
        format!("{base}?os=web&language=ko"),
        format!("{base}?os=web"),
    ]
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

/// Language label by dominant script: more Hangul than Latin letters => "ko".
/// Run this on the rendered body TEXT, not the raw JSON (whose English keys
/// would skew the count).
fn detect_lang(s: &str) -> &'static str {
    let mut ko = 0usize;
    let mut en = 0usize;
    for c in s.chars() {
        if ('\u{AC00}'..='\u{D7A3}').contains(&c) {
            ko += 1;
        } else if c.is_ascii_alphabetic() {
            en += 1;
        }
    }
    if ko > en {
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

/// First string value for `key` anywhere in the JSON body (depth-first).
fn json_field(body: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    find_field(&v, key)
}

fn find_field(v: &serde_json::Value, key: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            map.values().find_map(|x| find_field(x, key))
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(|x| find_field(x, key)),
        _ => None,
    }
}

/// Strip HTML to readable text: block/break tags become newlines, other tags
/// are dropped, then HTML entities are unescaped. Good enough for notice bodies.
fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            for n in chars.by_ref() {
                if n == '>' {
                    break;
                }
                tag.push(n);
            }
            let name: String = tag
                .trim()
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol"
            ) {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    // Collapse runs of 3+ newlines to 2 and trim trailing spaces per line.
    let unescaped = unescape(&out);
    let mut result = String::new();
    let mut blank = 0;
    for line in unescaped.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank <= 1 {
                result.push('\n');
            }
        } else {
            blank = 0;
            result.push_str(line.trim_start());
            result.push('\n');
        }
    }
    result
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&") // keep last so "&amp;lt;" doesn't double-decode
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
    let mut body = false;
    let mut probe = false;
    let mut match_term: Option<String> = None;

    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--raw" => raw = true,
            "--body" => body = true,
            "--probe" => probe = true,
            "-h" | "--help" => {
                println!(
                    "usage: upbit-notice [id|url] [--lang en|ko] [--body] [--match PHRASE] [--probe] [--raw]\n\
                     \n\
                     Prints the notice title to stdout (English by default).\n\
                     Diagnostics and any --match results go to stderr.\n\
                     \n\
                       --body          also print the notice body as plain text\n\
                       --match PHRASE  also report occurrences of PHRASE (case-insensitive)\n\
                       --lang en|ko    language (default en)\n\
                       --probe         try all endpoint variants and show a diagnostic table\n\
                       --raw           dump the raw JSON to stderr"
                );
                std::process::exit(0);
            }
            "--lang" | "-l" => {
                let v = it.next().ok_or("--lang needs a value (en|ko)")?;
                lang = parse_lang(&v)?;
            }
            "--match" | "-m" => {
                match_term = Some(it.next().ok_or("--match needs a PHRASE")?);
            }
            s if s.starts_with("--lang=") => lang = parse_lang(&s[7..])?,
            s if s.starts_with("--match=") => match_term = Some(s[8..].to_string()),
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

    Ok(Opts { target, is_url, lang, raw, body, probe, match_term })
}

fn parse_lang(s: &str) -> Result<Lang, String> {
    match s.to_ascii_lowercase().as_str() {
        "en" | "english" => Ok(Lang::En),
        "ko" | "kr" | "korean" => Ok(Lang::Ko),
        other => Err(format!("unknown lang '{other}' (use en|ko)")),
    }
}
