use crate::error::TcpKaliError;
use crate::utils::{generate_payload, get_file_arg, get_message_arg, parse_duration, parse_rate};
use bytes::Bytes;
use clap::{Arg, ArgAction, Command, value_parser};
use std::sync::Arc;
use std::time::Duration;

// Larger batches reduce pipeline bookkeeping. The number of batches allowed
// in flight is derived from the effective batch size below.
pub(crate) const PIPELINE_BATCH_BYTES: usize = 16 * 1024;
const PIPELINE_WINDOW_BYTES: usize = 32 * 1024;
const PIPELINE_WINDOW_BATCHES: usize = 4;
const MAX_PIPELINE_BATCH_MESSAGES: usize = 256;

/// Load test configuration
/// Uses 64-byte alignment to optimize cache line efficiency
#[repr(align(64))]
#[derive(Clone, Debug)]
pub struct Config {
    /// Test duration
    pub duration: Duration,
    /// Warmup duration
    pub warmup_duration: Duration,
    /// Message size (bytes)
    pub message_size: usize,
    /// Whether quiet mode is enabled
    pub quiet: bool,
    /// Whether Nagle algorithm is enabled
    pub nagle: bool,
    /// Whether pipeline mode is enabled
    pub pipeline: bool,
    /// Number of connections
    pub connections: u64,
    /// Connection rate (connections per second)
    pub connect_rate: u64,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Channel lifetime
    pub channel_lifetime: Option<Duration>,
    /// First message
    pub first_message: Option<Bytes>,
    /// Test message
    pub message: Option<Bytes>,
    /// Repeated TCP payload shared by all pipeline connections
    pub pipeline_message: Option<Bytes>,
    /// Number of protocol messages in `pipeline_message`
    pub pipeline_batch_size: usize,
    /// Message sending rate (messages per second)
    pub message_rate: Option<u64>,
    /// Whether WebSocket is used
    pub use_websocket: bool,
    /// Output CSV file path
    pub output: Option<String>,
}

/// Parse command line arguments and create configuration
///
/// # Arguments
/// * `matches` - Command line argument matches
///
/// # Returns
/// * `Arc<Config>` - Shared configuration object
pub fn parse_config(matches: &clap::ArgMatches) -> Result<Arc<Config>, TcpKaliError> {
    let unescape = matches.get_flag("unescape-message-args");

    let message_size = *matches.get_one::<usize>("message-size").unwrap();

    let message = get_message_arg(matches, "message", unescape)
        .or_else(|| get_file_arg(matches, "message-file", unescape))
        .or_else(|| generate_payload(message_size));
    let (pipeline_message, pipeline_batch_size) = build_pipeline_message(message.as_ref());

    let config = Config {
        duration: *matches.get_one::<Duration>("duration").unwrap(),
        warmup_duration: *matches.get_one::<Duration>("warmup").unwrap(),
        quiet: matches.get_flag("quiet"),
        nagle: matches.get_flag("nagle"),
        pipeline: matches.get_flag("pipeline"),
        connections: *matches.get_one::<u64>("connections").unwrap(),
        connect_rate: *matches.get_one::<u64>("connect-rate").unwrap(),
        connect_timeout: *matches.get_one::<Duration>("connect-timeout").unwrap(),
        channel_lifetime: matches.get_one::<Duration>("channel-lifetime").cloned(),
        first_message: get_message_arg(matches, "first-message", unescape)
            .or_else(|| get_file_arg(matches, "first-message-file", unescape)),
        message,
        pipeline_message,
        pipeline_batch_size,
        message_size,
        message_rate: matches.get_one::<u64>("message-rate").cloned(),
        use_websocket: matches.get_flag("websocket"),
        output: matches.get_one::<String>("output").cloned(),
    };

    // Parameter validation
    if config.connections == 0 {
        return Err(TcpKaliError::Config(
            "connections must be greater than 0".into(),
        ));
    }
    if config.connect_timeout.as_secs_f64() == 0.0 {
        return Err(TcpKaliError::Config(
            "connect-timeout must be greater than 0".into(),
        ));
    }
    if config.duration.as_secs_f64() == 0.0 {
        return Err(TcpKaliError::Config(
            "duration must be greater than 0".into(),
        ));
    }
    if let Some(lifetime) = config.channel_lifetime
        && lifetime.is_zero()
    {
        return Err(TcpKaliError::Config(
            "channel-lifetime must be greater than 0 if specified".into(),
        ));
    }

    Ok(Arc::new(config))
}

/// Returns the requested Tokio worker count, or half the logical CPUs when
/// the command line leaves it unspecified.
pub(crate) fn worker_count(matches: &clap::ArgMatches) -> usize {
    matches
        .get_one::<usize>("workers")
        .copied()
        .unwrap_or_else(default_worker_count)
}

fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .saturating_div(2)
        .max(1)
}

fn build_pipeline_message(message: Option<&Bytes>) -> (Option<Bytes>, usize) {
    let Some(message) = message else {
        return (None, 1);
    };
    if message.is_empty() {
        return (Some(Bytes::new()), 1);
    }

    let messages_per_batch =
        (PIPELINE_BATCH_BYTES / message.len()).clamp(1, MAX_PIPELINE_BATCH_MESSAGES);
    if messages_per_batch == 1 {
        return (Some(message.clone()), 1);
    }

    let mut batch = Vec::with_capacity(message.len() * messages_per_batch);
    for _ in 0..messages_per_batch {
        batch.extend_from_slice(message);
    }
    (Some(Bytes::from(batch)), messages_per_batch)
}

pub(crate) fn pipeline_in_flight_batches(message_size: usize, messages_per_batch: usize) -> usize {
    let batch_bytes = message_size.saturating_mul(messages_per_batch);
    if batch_bytes == 0 {
        return 1;
    }

    // Preserve the old window of four 8 KiB batches. Rate-limited partial
    // batches retain four permits, while full 16 KiB batches need only two.
    let messages_per_window_batch = (PIPELINE_WINDOW_BYTES
        .saturating_div(PIPELINE_WINDOW_BATCHES)
        .saturating_div(message_size))
    .clamp(1, MAX_PIPELINE_BATCH_MESSAGES)
    .min(messages_per_batch);
    let window_messages = messages_per_window_batch.saturating_mul(PIPELINE_WINDOW_BATCHES);
    window_messages.div_ceil(messages_per_batch).max(1)
}

/// Create command line argument parser
///
/// # Returns
/// * `clap::ArgMatches` - Parsed command line arguments
pub fn new_command() -> clap::ArgMatches {
    command().get_matches()
}

fn command() -> Command {
    Command::new("tcpkali2")
        .version(env!("CARGO_PKG_VERSION"))
        .about("A load testing tool for WebSocket and TCP server")
        .arg(
            Arg::new("host:port")
                .required(true)
                .num_args(1)
                .help("Target server in host:port format"),
        )
        .arg(
            Arg::new("websocket")
                .long("websocket")
                .alias("ws")
                .action(ArgAction::SetTrue)
                .help("Use RFC6455 WebSocket transport"),
        )
        .arg(
            Arg::new("connections")
                .short('c')
                .long("connections")
                .value_name("N")
                .default_value("1")
                .value_parser(value_parser!(u64))
                .help("Connections to keep open to the destinations"),
        )
        .arg(
            Arg::new("connect-rate")
                .long("connect-rate")
                .value_name("R")
                .default_value("100")
                .value_parser(parse_rate)
                .help("Limit number of new connections per second"),
        )
        .arg(
            Arg::new("connect-timeout")
                .long("connect-timeout")
                .value_name("T")
                .default_value("1s")
                .value_parser(parse_duration)
                .help("Limit time spent in a connection attempt"),
        )
        .arg(
            Arg::new("channel-lifetime")
                .long("channel-lifetime")
                .value_name("T")
                .value_parser(parse_duration)
                .help("Shut down each connection after T seconds"),
        )
        .arg(
            Arg::new("workers")
                .short('w')
                .long("workers")
                .value_name("N")
                .value_parser(value_parser!(usize))
                .help("Number of Tokio worker threads (default: max(1, logical CPUs / 2))"),
        )
        .arg(
            Arg::new("nagle")
                .long("nagle")
                .action(ArgAction::SetTrue)
                .help("Enable Nagle's algorithm (TCP_NODELAY is enabled by default)"),
        )
        .arg(
            Arg::new("pipeline")
                .short('p')
                .long("pipeline")
                .action(ArgAction::SetTrue)
                .help("Use pipeline client to send messages"),
        )
        .arg(
            Arg::new("duration")
                .short('T')
                .long("duration")
                .value_name("T")
                .default_value("15s")
                .value_parser(parse_duration)
                .help("Load test for the specified amount of time"),
        )
        .arg(
            Arg::new("warmup")
                .long("warmup")
                .value_name("T")
                .default_value("5s")
                .value_parser(parse_duration)
                .help("Warmup duration before benchmark (0 to skip)"),
        )
        .arg(
            Arg::new("unescape-message-args")
                .short('e')
                .long("unescape-message-args")
                .action(ArgAction::SetTrue)
                .help("Unescape the following {-m|-f|--first-*} arguments"),
        )
        .arg(
            Arg::new("first-message")
                .long("first-message")
                .value_name("string")
                .help("Send this message first, once"),
        )
        .arg(
            Arg::new("first-message-file")
                .long("first-message-file")
                .value_name("name")
                .help("Read the first message from a file"),
        )
        .arg(
            Arg::new("message")
                .short('m')
                .long("message")
                .value_name("string")
                .help("Message to repeatedly send to the remote"),
        )
        .arg(
            Arg::new("message-size")
                .short('s')
                .long("message-size")
                .default_value("128")
                .value_parser(value_parser!(usize))
                .help("Random message to repeatedly send to the remote"),
        )
        .arg(
            Arg::new("message-file")
                .short('f')
                .long("message-file")
                .value_name("name")
                .help("Read message to send from a file"),
        )
        .arg(
            Arg::new("message-rate")
                .short('r')
                .long("message-rate")
                .value_name("R")
                .value_parser(parse_rate)
                .help("Messages per second to send in a connection"),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .action(ArgAction::SetTrue)
                .help("Suppress real-time output"),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_name("FILE")
                .help("Export final results to a CSV file"),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        PIPELINE_BATCH_BYTES, build_pipeline_message, command, default_worker_count,
        pipeline_in_flight_batches, worker_count,
    };
    use bytes::Bytes;

    #[test]
    fn pipeline_batch_uses_sixteen_kibibytes() {
        let message = Bytes::from(vec![0x5a; 1024]);
        let (batch, message_count) = build_pipeline_message(Some(&message));
        let batch = batch.expect("pipeline batch");

        assert_eq!(message_count, 16);
        assert_eq!(batch.len(), PIPELINE_BATCH_BYTES);
        assert!(
            batch
                .chunks_exact(message.len())
                .all(|chunk| chunk == message.as_ref())
        );
    }

    #[test]
    fn pipeline_window_retains_four_rate_limited_batches() {
        assert_eq!(pipeline_in_flight_batches(1024, 5), 4);
    }

    #[test]
    fn pipeline_window_uses_two_full_sixteen_kibibyte_batches() {
        assert_eq!(pipeline_in_flight_batches(1024, 16), 2);
    }

    #[test]
    fn pipeline_window_accounts_for_the_message_count_cap() {
        assert_eq!(pipeline_in_flight_batches(16, 256), 4);
    }

    #[test]
    fn pipeline_window_retains_four_large_messages() {
        assert_eq!(pipeline_in_flight_batches(20 * 1024, 1), 4);
    }

    #[test]
    fn workers_default_to_half_available_parallelism() {
        let matches = command()
            .try_get_matches_from(["tcpkali2", "127.0.0.1:9000"])
            .unwrap();
        assert_eq!(worker_count(&matches), default_worker_count());
        assert!(worker_count(&matches) >= 1);
    }

    #[test]
    fn explicit_workers_override_the_dynamic_default() {
        let matches = command()
            .try_get_matches_from(["tcpkali2", "-w", "37", "127.0.0.1:9000"])
            .unwrap();
        assert_eq!(worker_count(&matches), 37);
    }
}
