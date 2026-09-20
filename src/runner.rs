//! Load test runner module
//!
//! Responsible for managing asynchronous task execution, statistics collection and result reporting

use crate::command::{Config, worker_count};
use crate::error::TcpKaliError;
use crate::stats::Stats;
use crate::tcp_worker::tcp_worker;
use crate::utils::duration_for_count;
use crate::websocket_worker::websocket_worker;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time;

/// Asynchronous main function responsible for executing load tests
///
/// # Arguments
/// - `matches`: Command line argument matches
///
/// # Returns
/// - `Result<(), TcpKaliError>`: Execution result
pub async fn async_main(matches: clap::ArgMatches) -> Result<(), TcpKaliError> {
    let config = crate::command::parse_config(&matches)?;
    let stats = Arc::new(Stats::new());
    let (load_start_tx, load_start_rx) = tokio::sync::watch::channel(false);
    let (benchmark_start_tx, benchmark_start_rx) = tokio::sync::watch::channel(None);
    let workers = worker_count(&matches);
    // Collect targets for CSV export (use first target for the CSV row)
    let targets: Vec<String> = matches
        .get_many::<String>("host:port")
        .unwrap()
        .cloned()
        .collect();

    // Periodically output real-time statistics
    let stats_printer = (!config.quiet).then(|| spawn_stats_printer(stats.clone()));

    let mut tasks = JoinSet::new();
    let connection_start = time::Instant::now();
    let mut spawned_connections = 0u64;
    let connection_batch_size = (config.connect_rate / 1_000).max(1);

    // Spawn progressively instead of creating thousands of sleeping tasks. An
    // absolute deadline preserves the requested aggregate connection rate.
    for target in targets.iter() {
        for _ in 0..config.connections {
            if config.connect_rate > 0
                && spawned_connections > 0
                && spawned_connections.is_multiple_of(connection_batch_size)
            {
                let deadline =
                    connection_start + duration_for_count(spawned_connections, config.connect_rate);
                time::sleep_until(deadline).await;
            }

            let task_config = config.clone();
            let task_stats = stats.clone();
            let target = target.clone();
            let task_load_start = load_start_rx.clone();
            let task_benchmark_start = config.channel_lifetime.map(|_| benchmark_start_rx.clone());

            tasks.spawn(async move {
                if task_config.use_websocket {
                    websocket_worker(
                        &target,
                        task_config,
                        task_stats,
                        task_load_start,
                        task_benchmark_start,
                    )
                    .await
                } else {
                    tcp_worker(
                        &target,
                        task_config,
                        task_stats,
                        task_load_start,
                        task_benchmark_start,
                    )
                    .await
                }
            });
            spawned_connections += 1;

            while let Some(result) = tasks.try_join_next() {
                report_task_result(result, config.quiet);
            }
        }
    }

    let _ = load_start_tx.send(true);

    // Warmup phase
    warmup_phase(&config, &stats).await;

    let start_time = time::Instant::now();
    let _ = benchmark_start_tx.send(Some(start_time));

    // Execute benchmark
    execute_benchmark(&config, &stats, &mut tasks).await;

    if let Some(stats_printer) = stats_printer {
        stats_printer.abort();
    }

    let elapsed = start_time.elapsed();

    // Output statistical results
    Stats::print_final_stats(&stats, elapsed, !config.quiet);

    // Export to CSV if --output is specified
    if let Some(ref path) = config.output {
        let target = targets.first().map(|s| s.as_str()).unwrap_or("");
        crate::csv_export::export_csv(path, &config, &stats, elapsed, target, workers);
    }

    Ok(())
}

/// Spawn statistics printer task
fn spawn_stats_printer(stats: Arc<Stats>) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Wait for warmup to complete
        while stats.is_warmup() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Reset the live window at the warmup boundary.
        let now = crate::utils::unix_timestamp_millis();
        stats
            .last_print_time
            .store(now, std::sync::atomic::Ordering::Relaxed);
        stats.last_print_count.store(
            stats
                .total_requests
                .load(std::sync::atomic::Ordering::Relaxed),
            std::sync::atomic::Ordering::Relaxed,
        );

        let mut interval = time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if stats.is_shutting_down() {
                break;
            }

            let qps = stats.get_qps();
            let current_count = stats.total_requests.load(Ordering::Relaxed);
            let hist = stats.latency_histogram.lock();
            let p50 = if hist.is_empty() {
                0
            } else {
                hist.value_at_percentile(50.0)
            };
            let p95 = if hist.is_empty() {
                0
            } else {
                hist.value_at_percentile(95.0)
            };
            let p99 = if hist.is_empty() {
                0
            } else {
                hist.value_at_percentile(99.0)
            };

            println!(
                "[Live] QPS: {:.0} | Req: {} | Latency(us): P50={} P95={} P99={}",
                qps, current_count, p50, p95, p99
            );
        }
    })
}

/// Warmup phase processing
async fn warmup_phase(config: &Config, stats: &Arc<Stats>) {
    // If warmup duration is 0, skip warmup phase
    if config.warmup_duration.as_secs_f64() == 0.0 {
        stats.end_warmup();
        return;
    }

    if !config.quiet {
        println!(
            "Warming up for {} seconds...",
            config.warmup_duration.as_secs()
        );
    }
    // Wait for warmup to complete
    time::sleep(config.warmup_duration).await;
    // Reset statistics
    stats.end_warmup();
    // Output log
    if !config.quiet {
        println!(
            "Warmup completed. Starting benchmark for {} seconds...",
            config.duration.as_secs()
        );
    }
}

/// Execute benchmark
async fn execute_benchmark(
    config: &Config,
    stats: &Arc<Stats>,
    tasks: &mut JoinSet<Result<(), TcpKaliError>>,
) {
    tokio::select! {
        _ = time::sleep(config.duration) => {}
        _ = tokio::signal::ctrl_c() => {}
    }

    // Mark benchmark as shutting down
    stats.set_shutting_down();
    tasks.abort_all();

    while let Some(result) = tasks.join_next().await {
        report_task_result(result, config.quiet);
    }
}

fn report_task_result(result: Result<Result<(), TcpKaliError>, JoinError>, quiet: bool) {
    match result {
        Ok(Err(error)) if !quiet => eprintln!("Task error: {}", error),
        Err(error) if !error.is_cancelled() && !quiet => eprintln!("Task join error: {}", error),
        _ => {}
    }
}
