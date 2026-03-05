//! Load test runner module
//!
//! Responsible for managing asynchronous task execution, statistics collection and result reporting

use crate::command::Config;
use crate::error::TcpKaliError;
use crate::stats::Stats;
use crate::tcp_worker::tcp_worker;
use crate::websocket_worker::websocket_worker;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
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
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let workers = *matches.get_one::<usize>("workers").unwrap();
    // Collect targets for CSV export (use first target for the CSV row)
    let targets: Vec<String> = matches
        .get_many::<String>("host:port")
        .unwrap()
        .cloned()
        .collect();

    // Periodically output real-time statistics
    if !config.quiet {
        spawn_stats_printer(stats.clone(), shutdown_rx.resubscribe());
    }

    let mut tasks = FuturesUnordered::new();

    // Optimize connection rate control: pre-calculate delay values to avoid repeated calculation per task
    let connect_delay = if config.connect_rate > 0 {
        Some(Duration::from_secs(1) / config.connect_rate as u32)
    } else {
        None
    };

    // Prepare tasks
    for target in targets.iter() {
        for i in 0..config.connections {
            let task_config = config.clone();
            let task_stats = stats.clone();
            let target = target.clone();
            let shutdown = shutdown_tx.subscribe();
            let connect_delay = connect_delay.clone();

            tasks.push(tokio::spawn(async move {
                // Apply connection rate control
                if let Some(delay) = connect_delay {
                    // Use index to stagger connection times, avoid all connections starting simultaneously
                    let staggered_delay = delay * (i % task_config.connect_rate.max(1) as u64) as u32;
                    time::sleep(staggered_delay).await;
                }

                if task_config.use_websocket {
                    websocket_worker(&target, task_config, task_stats, shutdown).await
                } else {
                    tcp_worker(&target, task_config, task_stats, shutdown).await
                }
            }));
        }
    }

    // Warmup phase
    warmup_phase(&config, &stats).await;

    let start_time = Instant::now();

    // Execute benchmark
    execute_benchmark(&config, &stats, &shutdown_tx, &mut tasks).await;

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
fn spawn_stats_printer(stats: Arc<Stats>, mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) {
    tokio::spawn(async move {
        // Wait for warmup to complete
        while stats.is_warmup() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        
        // Wait 1 second after warmup to give benchmark enough time to start
        tokio::time::sleep(Duration::from_secs(1)).await;
        
        // Reset statistics print time to avoid NaN in first statistics after warmup
        let now = crate::utils::unix_timestamp_millis();
        stats.last_print_time.store(now, std::sync::atomic::Ordering::Relaxed);
        stats.last_print_count.store(stats.total_requests.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
        
        let mut interval = time::interval(Duration::from_millis(1000));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let qps = stats.get_qps();
                    let current_count = stats.total_requests.load(Ordering::Relaxed);

                    let hist = stats.latency_histogram.lock();
                    // Check if histogram has data
                    let p50 = if hist.len() > 0 { hist.value_at_percentile(50.0) } else { 0 };
                    let p95 = if hist.len() > 0 { hist.value_at_percentile(95.0) } else { 0 };
                    let p99 = if hist.len() > 0 { hist.value_at_percentile(99.0) } else { 0 };
                    
                    println!(
                        "[Live] QPS: {:.0} | Req: {} | Latency(us): P50={} P95={} P99={}",
                        qps,
                        current_count,
                        p50,
                        p95,
                        p99
                    );
                }
                _ = shutdown_rx.recv() => break,
            }
        }
    });
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
    shutdown_tx: &tokio::sync::broadcast::Sender<()>,
    tasks: &mut FuturesUnordered<tokio::task::JoinHandle<Result<(), TcpKaliError>>>,
) {
    // Wait for benchmark to complete
    time::sleep(config.duration).await;
    // Mark benchmark as shutting down
    stats.set_shutting_down();
    // Notify benchmark end
    let _ = shutdown_tx.send(());
    // Wait for all tasks to complete
    while let Some(result) = tasks.next().await {
        match result {
            Ok(task_result) => {
                if let Err(e) = task_result {
                    if !config.quiet {
                        eprintln!("Task error: {}", e);
                    }
                }
            }
            Err(e) => {
                if !config.quiet {
                    eprintln!("Task join error: {}", e);
                }
            }
        }
    }
}