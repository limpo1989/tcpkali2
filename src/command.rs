use crate::error::TcpKaliError;
use crate::utils::{generate_payload, get_file_arg, get_message_arg, parse_duration, parse_rate};
use bytes::Bytes;
use clap::{value_parser, Arg, ArgAction, Command};
use std::sync::Arc;
use std::time::Duration;

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
        message: get_message_arg(matches, "message", unescape)
            .or_else(|| get_file_arg(matches, "message-file", unescape))
            .or_else(|| generate_payload(message_size)),
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
    if let Some(lifetime) = config.channel_lifetime {
        if lifetime.as_secs_f64() == 0.0 {
            return Err(TcpKaliError::Config(
                "channel-lifetime must be greater than 0 if specified".into(),
            ));
        }
    }

    Ok(Arc::new(config))
}

/// Create command line argument parser
///
/// # Returns
/// * `clap::ArgMatches` - Parsed command line arguments
pub fn new_command() -> clap::ArgMatches {
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
                .default_value("8")
                .value_parser(value_parser!(usize))
                .help("Number of Tokio worker threads to use"),
        )
        .arg(
            Arg::new("nagle")
                .long("nagle")
                .action(ArgAction::SetTrue)
                .help("Control Nagle algorithm (set TCP_NODELAY)"),
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
        .get_matches()
}
