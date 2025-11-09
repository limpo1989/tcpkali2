use bytes::Bytes;
use rand::Rng;
use rand::distr::Alphanumeric;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Duration string empty".to_string());
    }

    let sl = s.to_lowercase();

    // 先处理毫秒后缀
    if sl.ends_with("ms") {
        let num_str = sl.strip_suffix("ms").unwrap().trim();
        if num_str.is_empty() {
            return Err("Invalid duration number".to_string());
        }
        let num: u64 = num_str
            .parse()
            .map_err(|_| "Invalid duration number".to_string())?;
        return Ok(Duration::from_millis(num));
    }

    // 必须至少有一个数字和一个单位字符
    if sl.len() < 2 {
        return Err("Duration string too short".to_string());
    }

    // 单字符单位：s, m, h, d
    let (num_str, unit_str) = sl.split_at(sl.len() - 1);
    let num_str = num_str.trim();
    if num_str.is_empty() {
        return Err("Invalid duration number".to_string());
    }
    let num: u64 = num_str
        .parse()
        .map_err(|_| "Invalid duration number".to_string())?;

    match unit_str {
        "s" => Ok(Duration::from_secs(num)),
        "m" => Ok(Duration::from_secs(num.saturating_mul(60))),
        "h" => Ok(Duration::from_secs(num.saturating_mul(3600))),
        "d" => Ok(Duration::from_secs(num.saturating_mul(86_400))),
        _ => Err("Invalid duration unit".to_string()),
    }
}

pub fn parse_rate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty rate string".to_string());
    }

    let sl = s.to_lowercase();

    if let Some(num_str) = sl.strip_suffix('k') {
        let num: u64 = num_str
            .trim()
            .parse()
            .map_err(|_| format!("Invalid rate number: '{}'", num_str.trim()))?;
        return Ok(num.saturating_mul(1_000));
    }

    sl.parse::<u64>()
        .map_err(|_| format!("Invalid rate number: '{}'", sl))
}

pub fn unescape_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('\\') => result.push('\\'),
                Some('0') => result.push('\0'),
                Some('x') => {
                    // Handle hex escape sequences like \x41
                    let hex_digits: String = chars.by_ref().take(2).collect();
                    if hex_digits.len() == 2 {
                        if let Ok(byte) = u8::from_str_radix(&hex_digits, 16) {
                            result.push(byte as char);
                        } else {
                            result.push_str(&format!("\\x{}", hex_digits));
                        }
                    } else {
                        result.push_str("\\x");
                        result.push_str(&hex_digits);
                    }
                }
                Some(c) => {
                    // Unknown escape sequence, keep both characters
                    result.push('\\');
                    result.push(c);
                }
                None => result.push('\\'), // Backslash at end of string
            }
        } else {
            result.push(c);
        }
    }

    result
}

pub fn get_message_arg(
    matches: &clap::ArgMatches,
    arg_name: &str,
    unescape: bool,
) -> Option<Bytes> {
    matches.get_one::<String>(arg_name).map(|s| {
        if unescape {
            Bytes::from(unescape_string(s))
        } else {
            Bytes::copy_from_slice(s.as_bytes())
        }
    })
}

pub fn get_file_arg(matches: &clap::ArgMatches, arg_name: &str, unescape: bool) -> Option<Bytes> {
    matches.get_one::<String>(arg_name).and_then(|filename| {
        match std::fs::read_to_string(filename) {
            Ok(content) => {
                if unescape {
                    Some(Bytes::from(unescape_string(&content)))
                } else {
                    Some(Bytes::from(content))
                }
            }
            Err(e) => {
                eprintln!("Failed to read file {}: {}", filename, e);
                None
            }
        }
    })
}

pub fn generate_payload(size: usize) -> Option<Bytes> {
    Some(Bytes::from(
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(size)
            .collect::<Vec<u8>>(),
    ))
}

pub fn unix_timestamp_millis() -> u64 {
    let now = SystemTime::now();
    now.duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis() as u64
}
