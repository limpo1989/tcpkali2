use crate::command::{Config, pipeline_in_flight_batches};
use crate::error::TcpKaliError;
use crate::stats::{LocalStatsCache, Stats};
use crate::utils::{RatePacer, wait_for_benchmark_lifetime, wait_for_load_start};

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Semaphore, mpsc};
use tokio::time::{self, Instant};

const PIPELINE_READ_BUFFER_BYTES: usize = 8 * 1024;

macro_rules! log_error {
    ($stats:expr, $config:expr, $($arg:tt)*) => {
        if !$stats.is_shutting_down() && !$config.quiet {
            eprintln!($($arg)*);
        }
    };
}

struct SentBatch {
    sent_at: Instant,
    remaining: usize,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Runs one TCP connection. Global benchmark shutdown is handled by aborting
/// this task, so the per-message path does not poll a shared shutdown channel.
pub async fn tcp_worker(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    load_start: tokio::sync::watch::Receiver<bool>,
    benchmark_start: Option<tokio::sync::watch::Receiver<Option<Instant>>>,
) -> Result<(), TcpKaliError> {
    let worker = run_tcp_worker(target, &config, &stats, load_start);
    tokio::pin!(worker);

    if let Some(lifetime) = config.channel_lifetime {
        let benchmark_start = benchmark_start.expect("Benchmark start receiver must be provided");
        tokio::select! {
            result = &mut worker => result,
            _ = wait_for_benchmark_lifetime(benchmark_start, lifetime) => Ok(()),
        }
    } else {
        worker.await
    }
}

async fn run_tcp_worker(
    target: &str,
    config: &Config,
    stats: &Stats,
    load_start: tokio::sync::watch::Receiver<bool>,
) -> Result<(), TcpKaliError> {
    stats.total_connections.fetch_add(1, Ordering::Relaxed);

    let stream = match time::timeout(config.connect_timeout, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            log_error!(stats, config, "Failed to connect to {}: {}", target, error);
            stats.record_connection_error();
            return Ok(());
        }
        Err(_) => {
            log_error!(stats, config, "Connection timeout to {}", target);
            stats.record_connection_error();
            return Ok(());
        }
    };

    stream.set_nodelay(!config.nagle)?;
    let (mut reader, mut writer) = stream.into_split();

    if let Some(first_message) = &config.first_message {
        let sent_at = Instant::now();
        if let Err(error) = writer.write_all(first_message).await {
            log_error!(stats, config, "Failed to send first message: {}", error);
            stats.record_connection_error();
            return Ok(());
        }

        let mut response = vec![0; first_message.len()];
        if let Err(error) = reader.read_exact(&mut response).await {
            log_error!(
                stats,
                config,
                "Failed to read first message response: {}",
                error
            );
            stats.record_connection_error();
            return Ok(());
        }

        let latency_us = sent_at.elapsed().as_micros().max(1) as u64;
        stats.record_latency(latency_us, 0);
        stats.record_request(first_message.len(), first_message.len());
    }

    stats.success_connections.fetch_add(1, Ordering::Relaxed);
    wait_for_load_start(load_start).await;

    let message = config.message.as_ref().expect("Message must be provided");
    if message.is_empty() || config.message_rate == Some(0) {
        std::future::pending::<()>().await;
        return Ok(());
    }

    if config.pipeline {
        run_pipeline(reader, writer, config, stats).await
    } else {
        run_pingpong(reader, writer, config, stats).await
    }
}

async fn run_pingpong(
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    config: &Config,
    stats: &Stats,
) -> Result<(), TcpKaliError> {
    let message = config.message.as_ref().expect("Message must be provided");
    let mut response = vec![0; message.len()];
    let mut pacer = RatePacer::new(config.message_rate);
    let mut local_stats = LocalStatsCache::new(stats);

    loop {
        pacer.wait().await;
        let sent_at = Instant::now();

        if let Err(error) = writer.write_all(message).await {
            log_error!(stats, config, "Write error: {}", error);
            stats.record_connection_error();
            return Ok(());
        }

        if let Err(error) = reader.read_exact(&mut response).await {
            log_error!(stats, config, "Read error: {}", error);
            stats.record_connection_error();
            return Ok(());
        }

        local_stats.record_responses(
            1,
            message.len(),
            message.len(),
            sent_at.elapsed().as_micros().max(1) as u64,
        );
        pacer.advance(1);
    }
}

async fn run_pipeline(
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
    config: &Config,
    stats: &Stats,
) -> Result<(), TcpKaliError> {
    let (sent_tx, sent_rx) = mpsc::unbounded_channel();
    tokio::select! {
        result = pipeline_writer(writer, config, stats, sent_tx) => result,
        result = pipeline_reader(reader, config, stats, sent_rx) => result,
    }
}

async fn pipeline_writer(
    mut writer: OwnedWriteHalf,
    config: &Config,
    stats: &Stats,
    sent_tx: mpsc::UnboundedSender<SentBatch>,
) -> Result<(), TcpKaliError> {
    let message = config.message.as_ref().expect("Message must be provided");
    let pipeline_message = config
        .pipeline_message
        .as_ref()
        .expect("Pipeline message must be provided");
    let mut pacer = RatePacer::new(config.message_rate);
    let message_count = pacer.batch_size(config.pipeline_batch_size);
    let byte_count = message.len() * message_count;
    let window = Arc::new(Semaphore::new(pipeline_in_flight_batches(
        message.len(),
        message_count,
    )));

    loop {
        pacer.wait().await;
        let permit = window
            .clone()
            .acquire_owned()
            .await
            .expect("Pipeline semaphore must remain open");

        let sent_at = Instant::now();
        if sent_tx
            .send(SentBatch {
                sent_at,
                remaining: message_count,
                _permit: permit,
            })
            .is_err()
        {
            return Ok(());
        }

        if let Err(error) = writer.write_all(&pipeline_message[..byte_count]).await {
            log_error!(stats, config, "Write error: {}", error);
            stats.record_connection_error();
            return Ok(());
        }
        pacer.advance(message_count);
    }
}

async fn pipeline_reader(
    mut reader: OwnedReadHalf,
    config: &Config,
    stats: &Stats,
    mut sent_rx: mpsc::UnboundedReceiver<SentBatch>,
) -> Result<(), TcpKaliError> {
    let message_size = config
        .message
        .as_ref()
        .expect("Message must be provided")
        .len();
    let mut buffer = vec![0; PIPELINE_READ_BUFFER_BYTES.max(message_size)];
    let mut partial_response_bytes = 0usize;
    let mut current_batch: Option<SentBatch> = None;
    let mut local_stats = LocalStatsCache::new(stats);

    loop {
        let bytes_read = match reader.read(&mut buffer).await {
            Ok(0) => return Ok(()),
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                log_error!(stats, config, "Read error: {}", error);
                stats.record_connection_error();
                return Ok(());
            }
        };

        let available = partial_response_bytes + bytes_read;
        let mut response_count = available / message_size;
        partial_response_bytes = available % message_size;

        while response_count > 0 {
            let mut batch = match current_batch.take() {
                Some(batch) => batch,
                None => match sent_rx.recv().await {
                    Some(batch) => batch,
                    None => return Ok(()),
                },
            };

            let count = response_count.min(batch.remaining);
            local_stats.record_responses(
                count,
                message_size,
                message_size,
                batch.sent_at.elapsed().as_micros().max(1) as u64,
            );
            response_count -= count;

            batch.remaining -= count;
            if batch.remaining > 0 {
                current_batch = Some(batch);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tcp_worker;
    use crate::command::Config;
    use crate::stats::Stats;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pipeline_echo_batches_io_and_flushes_stats_on_cancel() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let bytes_read = stream.read(&mut buffer).await.unwrap();
                if bytes_read == 0 {
                    break;
                }
                stream.write_all(&buffer[..bytes_read]).await.unwrap();
            }
        });

        let payload = Bytes::from_static(b"echo payload");
        let messages_per_batch = 64;
        let pipeline_message = Bytes::from(payload.as_ref().repeat(messages_per_batch));
        let config = Arc::new(Config {
            duration: Duration::from_millis(200),
            warmup_duration: Duration::ZERO,
            message_size: payload.len(),
            quiet: true,
            nagle: false,
            pipeline: true,
            connections: 1,
            connect_rate: 0,
            connect_timeout: Duration::from_secs(1),
            channel_lifetime: None,
            first_message: None,
            message: Some(payload),
            pipeline_message: Some(pipeline_message),
            pipeline_batch_size: messages_per_batch,
            message_rate: Some(64_000),
            use_websocket: false,
            output: None,
        });
        let stats = Arc::new(Stats::new());
        stats.end_warmup();

        let worker_stats = stats.clone();
        let (_load_start_tx, load_start_rx) = tokio::sync::watch::channel(true);
        let worker = tokio::spawn(async move {
            tcp_worker(
                &address.to_string(),
                config,
                worker_stats,
                load_start_rx,
                None,
            )
            .await
            .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while stats.total_requests.load(Ordering::Relaxed) < 2_000 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("TCP echo worker did not produce responses");
        worker.abort();
        let _ = worker.await;
        server.abort();
        let _ = server.await;

        let responses = stats.total_requests.load(Ordering::Relaxed);
        assert!(responses >= 2_000, "only recorded {responses} responses");
        assert!(
            responses <= 10_000,
            "rate limiter allowed {responses} responses"
        );
    }
}
