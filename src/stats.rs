use crate::utils::unix_timestamp_millis;
use hdrhistogram::Histogram;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub struct Stats {
    pub total_connections: AtomicU64,
    pub success_connections: AtomicU64,
    pub total_requests: AtomicU64,
    pub total_bytes_sent: AtomicU64,
    pub total_bytes_received: AtomicU64,
    pub latency_histogram: parking_lot::Mutex<Histogram<u64>>,
    pub is_warmup: AtomicBool,
    pub is_shutting_down: AtomicBool,
    pub last_print_time: AtomicU64,
    pub last_print_count: AtomicU64,
    pub connection_errors: AtomicU64,
}

impl Stats {
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

    pub fn record_latency(&self, latency_us: u64, sample_count: usize) {
        if sample_count % 100 == 0 && !self.is_warmup() {
            let mut hist = self.latency_histogram.lock();
            hist.record(latency_us).unwrap_or_else(|e| {
                if !self.is_shutting_down() {
                    eprintln!("Failed to record latency: {}", e);
                }
            });
        }
    }

    pub fn record_request(&self, bytes_sent: usize, bytes_received: usize) {
        if !self.is_warmup() {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
            self.total_bytes_sent
                .fetch_add(bytes_sent as u64, Ordering::Relaxed);
            self.total_bytes_received
                .fetch_add(bytes_received as u64, Ordering::Relaxed);
        }
    }

    pub fn record_connection_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end_warmup(&self) {
        self.is_warmup.store(false, Ordering::Relaxed);
        self.total_requests.store(0, Ordering::Relaxed);
        self.total_bytes_sent.store(0, Ordering::Relaxed);
        self.total_bytes_received.store(0, Ordering::Relaxed);
        self.latency_histogram.lock().reset();
        self.last_print_count.store(0, Ordering::Relaxed);
        self.last_print_time
            .store(unix_timestamp_millis(), Ordering::Relaxed);
    }

    pub fn is_warmup(&self) -> bool {
        self.is_warmup.load(Ordering::Relaxed)
    }

    pub fn set_shutting_down(&self) {
        self.is_shutting_down.store(true, Ordering::Relaxed);
    }

    pub fn is_shutting_down(&self) -> bool {
        self.is_shutting_down.load(Ordering::Relaxed)
    }

    pub fn get_qps(&self) -> f64 {
        let now = unix_timestamp_millis();
        let current_count = self.total_requests.load(Ordering::Relaxed);
        let last_time = self.last_print_time.swap(now, Ordering::Relaxed);
        let last_count = self.last_print_count.swap(current_count, Ordering::Relaxed);

        let elapsed = (now - last_time) as f64 / 1000.0;
        (current_count - last_count) as f64 / elapsed
    }
}
