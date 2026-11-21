use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Compile a regex once, on first use.
#[macro_export]
macro_rules! re {
    ($s:expr) => {{
        static R: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new($s).unwrap());
        &*R
    }};
}

/// Lowercase, strip accents and punctuation, collapse spaces.
pub fn norm(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for c in s.nfd() {
        if unicode_normalization::char::is_combining_mark(c) { continue; }
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() { out.push(c); last_space = false; }
        else if !last_space { out.push(' '); last_space = true; }
    }
    out.trim_end().to_string()
}

pub fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}
pub fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
pub fn i(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => s.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0),
        _ => 0,
    }
}

pub fn tmdb_img(p: &str, size: &str) -> String {
    if p.is_empty() { String::new() } else { format!("https://image.tmdb.org/t/p/{size}{p}") }
}

pub fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

pub fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for c in s.chars() {
        if up && c.is_alphabetic() { out.extend(c.to_uppercase()); up = false; } else { out.push(c); }
        if c == ' ' || c == '-' || c == '(' { up = true; }
    }
    out
}
