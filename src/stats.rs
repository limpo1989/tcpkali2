use crate::utils::unix_timestamp_millis;
use hdrhistogram::Histogram;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const REQUEST_FLUSH_THRESHOLD: u64 = 256;
const LATENCY_FLUSH_THRESHOLD: usize = 64;
const LATENCY_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const LATENCY_SAMPLE_MASK: u64 = 0xFF;

pub(crate) fn rate_per_second(total: u64, duration: Duration) -> f64 {
    if duration.is_zero() {
        0.0
    } else {
        total as f64 / duration.as_secs_f64()
    }
}

pub(crate) fn megabits_per_second(total_bytes: u64, duration: Duration) -> f64 {
    rate_per_second(total_bytes.saturating_mul(8), duration) / 1_000_000.0
}

/// Per-connection statistics buffer. Drop flushes the last partial batch when a
/// worker is cancelled by the benchmark supervisor.
pub struct LocalStatsCache<'a> {
    stats: &'a Stats,
    /// Local request count
    requests: u64,
    /// Local bytes sent
    bytes_sent: u64,
    /// Local bytes received
    bytes_received: u64,
    response_sequence: u64,
    latency_samples: Vec<(u64, u64)>,
    latency_sample_count: usize,
    last_latency_flush: Instant,
}

impl<'a> LocalStatsCache<'a> {
    /// Create new local cache
    pub fn new(stats: &'a Stats) -> Self {
        Self {
            stats,
            requests: 0,
            bytes_sent: 0,
            bytes_received: 0,
            response_sequence: 0,
            latency_samples: Vec::with_capacity(LATENCY_FLUSH_THRESHOLD),
            latency_sample_count: 0,
            last_latency_flush: Instant::now(),
        }
    }

    /// Records responses with one warmup check and no global atomic updates on
    /// the common path. A single latency value represents responses read from
    /// the same pipeline batch.
    pub fn record_responses(
        &mut self,
        count: usize,
        bytes_sent_per_response: usize,
        bytes_received_per_response: usize,
        latency_us: u64,
    ) {
        if self.stats.is_warmup() {
            self.clear();
            return;
        }

        let count = count as u64;
        let previous_sequence = self.response_sequence;
        self.response_sequence = self.response_sequence.saturating_add(count);
        self.requests = self.requests.saturating_add(count);
        self.bytes_sent = self
            .bytes_sent
            .saturating_add((bytes_sent_per_response as u64).saturating_mul(count));
        self.bytes_received = self
            .bytes_received
            .saturating_add((bytes_received_per_response as u64).saturating_mul(count));

        let samples = (self.response_sequence >> 8) - (previous_sequence >> 8);
        if samples > 0 {
            self.latency_samples.push((latency_us, samples));
            self.latency_sample_count = self.latency_sample_count.saturating_add(samples as usize);
        }

        if self.requests >= REQUEST_FLUSH_THRESHOLD {
            self.flush_counters();
        }
        if samples > 0
            && (self.latency_sample_count >= LATENCY_FLUSH_THRESHOLD
                || self.last_latency_flush.elapsed() >= LATENCY_FLUSH_INTERVAL)
        {
            self.flush_latencies();
        }
    }

    /// Commit local cache to global statistics
    pub fn flush(&mut self) {
        self.flush_counters();
        self.flush_latencies();
    }

    fn flush_counters(&mut self) {
        if self.requests > 0 {
            self.stats
                .total_requests
                .fetch_add(self.requests, Ordering::Relaxed);
            self.stats
                .total_bytes_sent
                .fetch_add(self.bytes_sent, Ordering::Relaxed);
            self.stats
                .total_bytes_received
                .fetch_add(self.bytes_received, Ordering::Relaxed);
        }

        self.requests = 0;
        self.bytes_sent = 0;
        self.bytes_received = 0;
    }

    fn flush_latencies(&mut self) {
        if !self.latency_samples.is_empty() {
            {
                let mut histogram = self.stats.latency_histogram.lock();
                for (latency_us, count) in self.latency_samples.drain(..) {
                    if let Err(error) = histogram.record_n(latency_us, count)
                        && !self.stats.is_shutting_down()
                    {
                        eprintln!("Failed to record latency: {}", error);
                    }
                }
            }
            self.latency_sample_count = 0;
            self.last_latency_flush = Instant::now();
        }
    }

    fn clear(&mut self) {
        self.requests = 0;
        self.bytes_sent = 0;
        self.bytes_received = 0;
        self.response_sequence = 0;
        self.latency_samples.clear();
        self.latency_sample_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LATENCY_FLUSH_THRESHOLD, LocalStatsCache, Stats, megabits_per_second, rate_per_second,
    };
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn local_cache_flushes_counters_and_latency_on_drop() {
        let stats = Stats::new();
        stats.end_warmup();

        {
            let mut cache = LocalStatsCache::new(&stats);
            cache.record_responses(256, 10, 20, 42);
            cache.record_responses(3, 10, 20, 84);
        }

        assert_eq!(stats.total_requests.load(Ordering::Relaxed), 259);
        assert_eq!(stats.total_bytes_sent.load(Ordering::Relaxed), 2_590);
        assert_eq!(stats.total_bytes_received.load(Ordering::Relaxed), 5_180);
        let histogram = stats.latency_histogram.lock();
        assert_eq!(histogram.len(), 1);
        assert_eq!(histogram.value_at_percentile(50.0), 42);
    }

    #[test]
    fn local_cache_does_not_carry_warmup_data_into_benchmark() {
        let stats = Stats::new();
        let mut cache = LocalStatsCache::new(&stats);
        cache.record_responses(100, 10, 20, 42);
        stats.end_warmup();
        cache.record_responses(1, 10, 20, 84);
        drop(cache);

        assert_eq!(stats.total_requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn local_cache_batches_latency_histogram_updates() {
        let stats = Stats::new();
        stats.end_warmup();
        let mut cache = LocalStatsCache::new(&stats);

        for _ in 1..LATENCY_FLUSH_THRESHOLD {
            cache.record_responses(256, 1, 1, 42);
        }
        assert!(stats.latency_histogram.lock().is_empty());

        cache.record_responses(256, 1, 1, 42);
        let histogram = stats.latency_histogram.lock();
        assert_eq!(histogram.len(), LATENCY_FLUSH_THRESHOLD as u64);
        assert_eq!(histogram.value_at_percentile(50.0), 42);
    }

    #[test]
    fn connection_error_rate_uses_connection_attempts() {
        let stats = Stats::new();
        stats.total_connections.store(4, Ordering::Relaxed);
        stats.connection_errors.store(3, Ordering::Relaxed);

        assert_eq!(stats.connection_error_rate(), 75.0);
    }

    #[test]
    fn rates_are_normalized_by_duration() {
        let duration = Duration::from_secs(2);
        assert_eq!(rate_per_second(1_000, duration), 500.0);
        assert_eq!(megabits_per_second(1_000_000, duration), 4.0);
        assert_eq!(rate_per_second(1_000, Duration::ZERO), 0.0);
    }
}

impl Drop for LocalStatsCache<'_> {
    fn drop(&mut self) {
        if self.stats.is_warmup() {
            self.clear();
        } else {
            self.flush();
        }
    }
}

/// Performance statistics data structure
/// Uses 64-byte alignment to optimize cache line efficiency and reduce false sharing
#[repr(align(64))]
#[derive(Debug)]
pub struct Stats {
    /// Total connections
    pub total_connections: AtomicU64,
    /// Successful connections
    pub success_connections: AtomicU64,
    /// Total requests
    pub total_requests: AtomicU64,
    /// Total bytes sent
    pub total_bytes_sent: AtomicU64,
    /// Total bytes received
    pub total_bytes_received: AtomicU64,
    /// Latency histogram
    pub latency_histogram: parking_lot::Mutex<Histogram<u64>>,
    /// Whether in warmup phase
    pub is_warmup: AtomicBool,
    /// Whether shutting down
    pub is_shutting_down: AtomicBool,
    /// Last print time
    pub last_print_time: AtomicU64,
    /// Last print count
    pub last_print_count: AtomicU64,
    /// Connection errors
    pub connection_errors: AtomicU64,
}

impl Stats {
    /// Create new statistics object
    ///
    /// # Returns
    /// * `Self` - New statistics object
    pub fn new() -> Self {
        let hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .expect("Failed to create histogram");

        Self {
            total_connections: AtomicU64::new(0),
            success_connections: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            total_bytes_sent: AtomicU64::new(0),
            total_bytes_received: AtomicU64::new(0),
            latency_histogram: parking_lot::Mutex::new(hist),
            is_warmup: AtomicBool::new(true),
            is_shutting_down: AtomicBool::new(false),
            last_print_time: AtomicU64::new(unix_timestamp_millis()),
            last_print_count: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
        }
    }

    /// Record latency data
    ///
    /// # Arguments
    /// * `latency_us` - Latency in microseconds
    /// * `sample_count` - Sample count
    pub fn record_latency(&self, latency_us: u64, sample_count: usize) {
        // Optimize sampling strategy: use bit operations to check if it's a multiple of a power of two, reducing branch prediction failures
        // Increase sampling interval to 256 to reduce lock contention
        if (sample_count as u64 & LATENCY_SAMPLE_MASK) == 0 && !self.is_warmup() {
            let mut hist = self.latency_histogram.lock();
            hist.record(latency_us).unwrap_or_else(|e| {
                if !self.is_shutting_down() {
                    eprintln!("Failed to record latency: {}", e);
                }
            });
        }
    }

    /// Record request data
    ///
    /// # Arguments
    /// * `bytes_sent` - Bytes sent
    /// * `bytes_received` - Bytes received
    pub fn record_request(&self, bytes_sent: usize, bytes_received: usize) {
        if !self.is_warmup() {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
            self.total_bytes_sent
                .fetch_add(bytes_sent as u64, Ordering::Relaxed);
            self.total_bytes_received
                .fetch_add(bytes_received as u64, Ordering::Relaxed);
        }
    }

    /// Record connection error
    ///
    /// Increments the connection error counter by one.
    pub fn record_connection_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_error_rate(&self) -> f64 {
        let total_connections = self.total_connections.load(Ordering::Relaxed);
        if total_connections == 0 {
            0.0
        } else {
            self.connection_errors.load(Ordering::Relaxed) as f64 / total_connections as f64 * 100.0
        }
    }

    /// End warmup phase
    ///
    /// Resets statistics counters and clears latency histogram to start the main benchmark phase.
    pub fn end_warmup(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        self.total_bytes_sent.store(0, Ordering::Relaxed);
        self.total_bytes_received.store(0, Ordering::Relaxed);
        self.latency_histogram.lock().reset();
        self.last_print_count.store(0, Ordering::Relaxed);
        self.last_print_time
            .store(unix_timestamp_millis(), Ordering::Relaxed);
        self.is_warmup.store(false, Ordering::Relaxed);
    }

    /// Check if currently in warmup phase
    ///
    /// # Returns
    /// * `bool` - True if in warmup phase, false otherwise
    pub fn is_warmup(&self) -> bool {
        self.is_warmup.load(Ordering::Relaxed)
    }

    /// Set shutting down flag
    ///
    /// Marks the statistics instance as shutting down, which prevents further error logging.
    pub fn set_shutting_down(&self) {
        self.is_shutting_down.store(true, Ordering::Relaxed);
    }

    /// Check if shutting down
    ///
    /// # Returns
    /// * `bool` - True if shutting down, false otherwise
    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Relaxed)
    }

    /// Get current queries per second (QPS)
    ///
    /// Calculates the QPS based on the request count since the last call.
    ///
    /// # Returns
    /// * `f64` - Queries per second
    pub fn get_qps(&self) -> f64 {
        let now = unix_timestamp_millis();
        let current_count = self.total_requests.load(Ordering::Relaxed);
        let last_time = self.last_print_time.swap(now, Ordering::Relaxed);
        let last_count = self.last_print_count.swap(current_count, Ordering::Relaxed);

        let elapsed_ms = now - last_time;

        // Avoid division by zero: if time interval is less than 1ms, return 0.0
        if elapsed_ms < 1 {
            return 0.0;
        }

        let elapsed = elapsed_ms as f64 / 1000.0;
        (current_count - last_count) as f64 / elapsed
    }

    /// Print final statistics results
    ///
    /// # Arguments
    /// - `stats`: Statistics data structure
    /// - `duration`: Test duration
    /// - `show_output`: Whether to display output
    pub fn print_final_stats(stats: &Stats, duration: Duration, show_output: bool) {
        if !show_output {
            return;
        }

        let hist = stats.latency_histogram.lock();
        let total_bytes_sent = stats.total_bytes_sent.load(Ordering::Relaxed);
        let total_bytes_received = stats.total_bytes_received.load(Ordering::Relaxed);
        let total_bytes = total_bytes_sent + total_bytes_received;
        let total_requests = stats.total_requests.load(Ordering::Relaxed);
        let qps = rate_per_second(total_requests, duration);
        let success_connections = stats.success_connections.load(Ordering::Relaxed) as f64;
        let total_connections = stats.total_connections.load(Ordering::Relaxed) as f64;
        let success_rate = if total_connections > 0.0 {
            success_connections / total_connections * 100.0
        } else {
            0.0
        };
        let connection_errors = stats.connection_errors.load(Ordering::Relaxed);
        let connection_error_rate = stats.connection_error_rate();
        let bandwidth = rate_per_second(total_bytes, duration) / 1_000_000.0;
        let traffic_down_mbps = megabits_per_second(total_bytes_received, duration);
        let traffic_up_mbps = megabits_per_second(total_bytes_sent, duration);

        println!("\n=== Final Results ===");
        println!("Duration:          {:.2}s", duration.as_secs_f64());
        println!("Total Connections: {}", total_connections);
        println!("Success Rate:      {:.1}%", success_rate);
        println!("Total Requests:    {}", total_requests);
        println!("Connection Errors: {}", connection_errors);
        println!("Connection Error Rate: {:.2}%", connection_error_rate);
        println!("Requests Rate:     {:.2} req/s", qps,);
        println!(
            "Throughput:        {:.2} MB",
            total_bytes as f64 / 1_000_000.0
        );
        println!("Bandwidth:         {:.2} MB/s", bandwidth);
        println!(
            "Traffic:           {:.2}↓, {:.2}↑ Mbps",
            traffic_down_mbps, traffic_up_mbps
        );
        println!("Latency Distribution (us):");
        println!("  Avg: {:8.1}  Min: {:8}", hist.mean(), hist.min());
        println!(
            "  P50: {:8}  P90: {:8}",
            hist.value_at_percentile(50.0),
            hist.value_at_percentile(90.0)
        );
        println!(
            "  P95: {:8}  P99: {:8}",
            hist.value_at_percentile(95.0),
            hist.value_at_percentile(99.0)
        );
        println!("  Max: {:8}", hist.max());
    }
}
