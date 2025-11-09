use crate::command::Config;
use crate::stats::Stats;

use crossbeam_queue::SegQueue;
use futures::{SinkExt, StreamExt};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::time;
use tokio_tungstenite::connect_async_with_config;
use tungstenite::Message;

pub async fn websocket_worker(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if config.pipeline {
        websocket_worker_pipeline(target, config, stats, shutdown).await
    } else {
        websocket_worker_pingpong(target, config, stats, shutdown).await
    }
}

pub async fn websocket_worker_pingpong(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    stats.total_connections.fetch_add(1, Ordering::Relaxed);

    // Connect to WebSocket server
    let ws_stream = match time::timeout(
        config.connect_timeout,
        connect_async_with_config(target, None, !config.nagle),
    )
    .await
    {
        Ok(Ok(ws)) => ws.0,
        Ok(Err(e)) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to connect to WebSocket {}: {}", target, e);
            }
            stats.record_connection_error();
            return Ok(());
        }
        Err(_) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("WebSocket connection timeout to {}", target);
            }
            stats.record_connection_error();
            return Ok(());
        }
    };

    let (mut write, mut read) = ws_stream.split();

    // Handle first message if configured
    if let Some(first_msg) = &config.first_message {
        let start = Instant::now();
        if let Err(e) = write.send(Message::Binary(first_msg.clone())).await {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to send first WebSocket message: {}", e);
            }
            stats.record_connection_error();
            return Ok(());
        }

        // Wait for first message response
        match read.next().await {
            Some(Ok(Message::Binary(data))) => {
                let latency = start.elapsed().as_micros() as u64;
                stats.record_latency(latency, 0);
                stats.record_request(first_msg.len(), data.len());
            }
            Some(Err(e)) => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Failed to read first message response: {}", e);
                }
                stats.record_connection_error();
                return Ok(());
            }
            _ => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Unexpected response to first message");
                }
                stats.record_connection_error();
                return Ok(());
            }
        }
    }

    // Prepare payload for main benchmark
    let message = config.message.as_ref().unwrap();

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

                    // Perform write operation
                    if let Err(e) = write.send(Message::Binary(message.clone())).await {
                        if !stats.is_shutting_down() && !config.quiet {
                            eprintln!("WebSocket send error: {}", e);
                        }
                        stats.record_connection_error();
                        return;
                    }

                    counter += 1;

                    if let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Binary(data)) => {
                                let latency = send_time.elapsed().as_micros() as u64;
                                stats.record_latency(latency, counter);
                                stats.record_request(message.len(), data.len());
                            }
                            Ok(_) => {}
                            Err(e) => {
                                if !stats.is_shutting_down() && !config.quiet {
                                    eprintln!("WebSocket receive error: {}", e);
                                }
                                stats.record_connection_error();
                                return;
                            }
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
    let _ = write.send(Message::Close(None)).await;
    drop(write);
    stats.success_connections.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub async fn websocket_worker_pipeline(
    target: &str,
    config: Arc<Config>,
    stats: Arc<Stats>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    stats.total_connections.fetch_add(1, Ordering::Relaxed);

    // Connect to WebSocket server
    let ws_stream = match time::timeout(
        config.connect_timeout,
        connect_async_with_config(target, None, !config.nagle),
    )
    .await
    {
        Ok(Ok(ws)) => ws.0,
        Ok(Err(e)) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to connect to WebSocket {}: {}", target, e);
            }
            stats.record_connection_error();
            return Ok(());
        }
        Err(_) => {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("WebSocket connection timeout to {}", target);
            }
            stats.record_connection_error();
            return Ok(());
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let sent_times = Arc::new(SegQueue::<Instant>::new());

    // Handle first message if configured
    if let Some(first_msg) = &config.first_message {
        let start = Instant::now();
        if let Err(e) = write.send(Message::Binary(first_msg.clone())).await {
            if !stats.is_shutting_down() && !config.quiet {
                eprintln!("Failed to send first WebSocket message: {}", e);
            }
            stats.record_connection_error();
            return Ok(());
        }

        // Wait for first message response
        match read.next().await {
            Some(Ok(Message::Binary(data))) => {
                let latency = start.elapsed().as_micros() as u64;
                stats.record_latency(latency, 0);
                stats.record_request(first_msg.len(), data.len());
            }
            Some(Err(e)) => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Failed to read first message response: {}", e);
                }
                stats.record_connection_error();
                return Ok(());
            }
            _ => {
                if !stats.is_shutting_down() && !config.quiet {
                    eprintln!("Unexpected response to first message");
                }
                stats.record_connection_error();
                return Ok(());
            }
        }
    }

    // Spawn reader task for full-duplex operation
    let reader_stats = stats.clone();
    let reader_config = config.clone();
    let reader_sent_times = sent_times.clone();
    let mut counter: usize = 0;

    let reader_handle = tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    counter += 1;
                    if let Some(sent_time) = reader_sent_times.pop() {
                        let latency = sent_time.elapsed().as_micros() as u64;
                        reader_stats.record_latency(latency, counter);
                        reader_stats.record_request(reader_config.message_size, data.len());
                    }
                }
                Ok(_) => continue, // Ignore non-binary messages
                Err(e) => {
                    if !reader_stats.is_shutting_down() && !reader_config.quiet {
                        eprintln!("WebSocket receive error: {}", e);
                    }
                    reader_stats.record_connection_error();
                    break;
                }
            }
        }
    });

    // Prepare payload for main benchmark
    let message = config.message.as_ref().unwrap();

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

                // Perform write operation
                if let Err(e) = write.send(Message::Binary(message.clone())).await {
                    if !stats.is_shutting_down() && !config.quiet {
                        eprintln!("WebSocket send error: {}", e);
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
    let _ = write.send(Message::Close(None)).await;
    drop(write);
    let _ = reader_handle.await;

    stats.success_connections.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
