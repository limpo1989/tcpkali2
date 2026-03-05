use crate::command::Config;
use crate::stats::Stats;
use std::fs;
use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// All CSV column headers in a fixed order.
const HEADERS: &[&str] = &[
    "timestamp",
    "target",
    "connections",
    "connect_rate",
    "connect_timeout_s",
    "channel_lifetime_s",
    "duration_s",
    "warmup_s",
    "workers",
    "nagle",
    "pipeline",
    "websocket",
    "message_size",
    "message_rate",
    "total_connections",
    "success_rate_pct",
    "total_requests",
    "error_rate_pct",
    "requests_per_sec",
    "throughput_mb",
    "bandwidth_mb_s",
    "traffic_down_mbps",
    "traffic_up_mbps",
    "latency_avg_us",
    "latency_min_us",
    "latency_p50_us",
    "latency_p90_us",
    "latency_p95_us",
    "latency_p99_us",
    "latency_max_us",
];

/// Build the header line from the fixed column list.
fn header_line() -> String {
    HEADERS.join(",")
}

/// Build a data row from config and stats.
fn build_row(
    config: &Config,
    stats: &Stats,
    duration: Duration,
    target: &str,
    workers: usize,
) -> String {
    let hist = stats.latency_histogram.lock();
    let total_bytes_sent = stats.total_bytes_sent.load(Ordering::Relaxed);
    let total_bytes_received = stats.total_bytes_received.load(Ordering::Relaxed);
    let total_bytes = total_bytes_sent + total_bytes_received;
    let total_requests = stats.total_requests.load(Ordering::Relaxed);
    let elapsed = duration.as_secs_f64();

    let qps = if elapsed > 0.0 {
        total_requests as f64 / elapsed
    } else {
        0.0
    };

    let total_connections = stats.total_connections.load(Ordering::Relaxed) as f64;
    let success_connections = stats.success_connections.load(Ordering::Relaxed) as f64;
    let success_rate = if total_connections > 0.0 {
        success_connections / total_connections * 100.0
    } else {
        0.0
    };

    let error_rate = if total_requests > 0 {
        stats.connection_errors.load(Ordering::Relaxed) as f64 / (total_requests as f64 * 100.0)
    } else {
        0.0
    };

    let bandwidth = if elapsed > 0.0 {
        total_bytes as f64 / elapsed / 1_000_000.0
    } else {
        0.0
    };

    // Get current UTC timestamp in RFC 3339 format without external crate
    let timestamp = {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = d.as_secs();
        // Convert epoch seconds to date-time components
        let (year, month, day, hour, min, sec) = epoch_to_datetime(secs);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, hour, min, sec
        )
    };

    let channel_lifetime_str = config
        .channel_lifetime
        .map(|d| format!("{:.2}", d.as_secs_f64()))
        .unwrap_or_else(|| "-1".to_string());

    let message_rate_str = config
        .message_rate
        .map(|r| r.to_string())
        .unwrap_or_else(|| "-1".to_string());

    let values: Vec<String> = vec![
        timestamp,
        target.to_string(),
        config.connections.to_string(),
        config.connect_rate.to_string(),
        format!("{:.2}", config.connect_timeout.as_secs_f64()),
        channel_lifetime_str,
        format!("{:.2}", config.duration.as_secs_f64()),
        format!("{:.2}", config.warmup_duration.as_secs_f64()),
        workers.to_string(),
        config.nagle.to_string(),
        config.pipeline.to_string(),
        config.use_websocket.to_string(),
        config.message_size.to_string(),
        message_rate_str,
        format!("{}", total_connections),
        format!("{:.1}", success_rate),
        total_requests.to_string(),
        format!("{:.2}", error_rate),
        format!("{:.2}", qps),
        format!("{:.2}", total_bytes as f64 / 1_000_000.0),
        format!("{:.2}", bandwidth),
        format!("{:.2}", total_bytes_received * 8 / 1_000_000),
        format!("{:.2}", total_bytes_sent * 8 / 1_000_000),
        format!("{:.1}", hist.mean()),
        format!("{}", hist.min()),
        format!("{}", hist.value_at_percentile(50.0)),
        format!("{}", hist.value_at_percentile(90.0)),
        format!("{}", hist.value_at_percentile(95.0)),
        format!("{}", hist.value_at_percentile(99.0)),
        format!("{}", hist.max()),
    ];

    values.join(",")
}

/// Convert epoch seconds (UTC) to (year, month, day, hour, minute, second).
fn epoch_to_datetime(epoch: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = epoch % 60;
    let min = (epoch / 60) % 60;
    let hour = (epoch / 3600) % 24;
    let mut days = epoch / 86400;

    // Calculate year
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    // Calculate month and day
    let days_in_months: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0u64;
    for (i, &dim) in days_in_months.iter().enumerate() {
        if days < dim {
            month = i as u64 + 1;
            break;
        }
        days -= dim;
    }
    let day = days + 1;

    (year, month, day, hour, min, sec)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Export benchmark results to a CSV file.
///
/// - If the file does not exist, creates it with headers + data row.
/// - If it exists and headers match, appends the data row.
/// - If it exists but headers don't match, silently overwrites with correct headers + data row.
pub fn export_csv(
    path: &str,
    config: &Config,
    stats: &Stats,
    duration: Duration,
    target: &str,
    workers: usize,
) {
    let expected_header = header_line();
    let row = build_row(config, stats, duration, target, workers);

    if let Ok(file) = fs::File::open(path) {
        // File exists — check if headers match
        let reader = std::io::BufReader::new(file);
        if let Some(Ok(first_line)) = reader.lines().next() {
            if first_line.trim() == expected_header {
                // Headers match — append
                if let Ok(mut f) = fs::OpenOptions::new().append(true).open(path) {
                    let _ = writeln!(f, "{}", row);
                    return;
                }
            }
        }
        // Headers don't match or couldn't read — fall through to overwrite
    }

    // Create / overwrite
    if let Ok(mut f) = fs::File::create(path) {
        let _ = writeln!(f, "{}", expected_header);
        let _ = writeln!(f, "{}", row);
    }
}
