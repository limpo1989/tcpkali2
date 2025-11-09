use crate::command::Config;
use crate::stats::Stats;

use crossbeam_queue::SegQueue;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

pub async fn tcp_worker(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if config.pipeline {
        tcp_worker_pipeline(target, config, stats, shutdown).await
    } else {
        tcp_worker_pingpong(target, config, stats, shutdown).await
    }
}

pub async fn tcp_worker_pingpong(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    stats.total_connections.fetch_add(1, Ordering::Relaxed);

    // Connect to server
    let stream = match time::timeout(config.connect_timeout, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to connect to {}: {}", target, e);
            }
            stats.record_connection_error();
            return Ok(());
        }
        Err(_) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Connection timeout to {}", target);
            }
            stats.record_connection_error();
            return Ok(());
        }
    };

    // 是否启用Nagle algorithm
    if config.nagle {
        stream.set_nodelay(false)?;
    }

    let (mut reader, mut writer) = stream.into_split();

    // Handle first message if configured
    if let Some(first_msg) = &config.first_message {
        let start = Instant::now();
        if let Err(e) = writer.write_all(first_msg).await {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to send first message: {}", e);
            }
            stats.record_connection_error();
            return Ok(());
        }

        // Wait for first message response
        let mut response_buf = vec![0u8; first_msg.len()];
        match reader.read_exact(&mut response_buf).await {
            Ok(_) => {
                let latency = start.elapsed().as_micros() as u64;
                stats.record_latency(latency, 0);
                stats.record_request(first_msg.len(), first_msg.len());
            }
            Err(e) => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Failed to read first message response: {}", e);
                }
                stats.record_connection_error();
                return Ok(());
            }
        }
    }

    // Prepare payload for main benchmark
    let message = config.message.as_ref().unwrap();

    let message_size = message.len();

    // Writer task
    let start_time = Instant::now();
    let mut counter = 0;

    let mut buf = vec![0u8; message_size];
    let expected_len = message_size;
    loop {
        if let Some(lifetime) = config.channel_lifetime {
            if start_time.elapsed() >= lifetime {
                break;
            }
        }

        tokio::select! {
            _ = async {
                if stats.is_shutting_down() {
                    return;
                }

                // Record send time before actual write
                let send_time = Instant::now();

                // Perform write operation immediately
                if let Err(e) = writer.write_all(message).await {
                    if !stats.is_shutting_down() && !config.quiet {
                        eprintln!("Write error: {}", e);
                    }
                    stats.record_connection_error();
                    return;
                }

                counter += 1;

                match reader.read_exact(&mut buf[..expected_len]).await {
                Ok(n) if n == expected_len => {
                    let latency = send_time.elapsed().as_micros() as u64;
                    stats.record_latency(latency, counter);
                    stats.record_request(expected_len, expected_len);
                }
                Ok(_) => return, // Unexpected EOF
                Err(e) => {
                    if !stats.is_shutting_down() && !config.quiet {
                        eprintln!("Read error: {}", e);
                    }
                    stats.record_connection_error();
                    return;
                }
            }

            if counter % 100 == 0 {
                tokio::task::yield_now().await;
            }

            if let Some(rate) = config.message_rate {
                let target_duration = Duration::from_secs_f64(1.0 / rate as f64);
                let elapsed = send_time.elapsed();
                if elapsed < target_duration {
                    time::sleep(target_duration - elapsed).await;
                }
            }
            } => {},
            _ = shutdown.recv() => break,
        }
    }

    // Clean up reader task
    drop(writer);
    stats.success_connections.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub async fn tcp_worker_pipeline(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    stats.total_connections.fetch_add(1, Ordering::Relaxed);

    // Connect to server
    let stream = match time::timeout(config.connect_timeout, TcpStream::connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to connect to {}: {}", target, e);
            }
            stats.record_connection_error();
            return Ok(());
        }
        Err(_) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Connection timeout to {}", target);
            }
            stats.record_connection_error();
            return Ok(());
        }
    };

    // 是否启用Nagle algorithm
    if config.nagle {
        stream.set_nodelay(false)?;
    }

    let (mut reader, mut writer) = stream.into_split();
    let sent_times = Arc::new(SegQueue::<Instant>::new());

    // Handle first message if configured
    if let Some(first_msg) = &config.first_message {
        let start = Instant::now();
        if let Err(e) = writer.write_all(first_msg).await {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to send first message: {}", e);
            }
            stats.record_connection_error();
            return Ok(());
        }

        // Wait for first message response
        let mut response_buf = vec![0u8; first_msg.len()];
        match reader.read_exact(&mut response_buf).await {
            Ok(_) => {
                let latency = start.elapsed().as_micros() as u64;
                stats.record_latency(latency, 0);
                stats.record_request(first_msg.len(), first_msg.len());
            }
            Err(e) => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Failed to read first message response: {}", e);
                }
                stats.record_connection_error();
                return Ok(());
            }
        }
    }

    // Prepare payload for main benchmark
    let message = config.message.as_ref().unwrap();

    let message_size = message.len();

    // Spawn reader task for full-duplex operation
    let reader_stats = stats.clone();
    let reader_config = config.clone();
    let reader_sent_times = sent_times.clone();
    let reader_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; message_size];
        let expected_len = message_size;
        let mut counter: usize = 0;

        loop {
            match reader.read_exact(&mut buf[..expected_len]).await {
                Ok(n) if n == expected_len => {
                    counter += 1;
                    if let Some(sent_time) = reader_sent_times.pop() {
                        let latency = sent_time.elapsed().as_micros() as u64;
                        reader_stats.record_request(expected_len, expected_len);
                        reader_stats.record_latency(latency, counter);
                    }
                }
                Ok(_) => break, // Unexpected EOF
                Err(e) => {
                    if !reader_stats.is_shutting_down() && !reader_config.quiet {
                        eprintln!("Read error: {}", e);
                    }
                    reader_stats.record_connection_error();
                    break;
                }
            }
        }
    });

    // Writer task
    let start_time = Instant::now();
    let mut counter = 0;

    loop {
        if let Some(lifetime) = config.channel_lifetime {
            if start_time.elapsed() >= lifetime {
                break;
            }
        }

        tokio::select! {
            _ = async {
                if stats.is_shutting_down() {
                    return;
                }

                // Record send time before actual write
                let send_time = Instant::now();
                sent_times.push(send_time);

                // Perform write operation immediately
                if let Err(e) = writer.write_all(message).await {
                    if !stats.is_shutting_down() && !config.quiet {
                        eprintln!("Write error: {}", e);
                    }
                    stats.record_connection_error();
                    return;
                }

                counter += 1;
                if counter % 100 == 0 {
                    tokio::task::yield_now().await;
                }

                if let Some(rate) = config.message_rate {
                    let target_duration = Duration::from_secs_f64(1.0 / rate as f64);
                    let elapsed = send_time.elapsed();
                    if elapsed < target_duration {
                        time::sleep(target_duration - elapsed).await;
                    }
                }
            } => {},
            _ = shutdown.recv() => break,
        }
    }

    // Clean up reader task
    drop(writer);
    let _ = reader_handle.await;

    stats.success_connections.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
