mod command;
mod csv_export;
mod error;
mod runner;
mod stats;
mod tcp_worker;
mod utils;
mod websocket_worker;

use crate::command::new_command;
use crate::error::TcpKaliError;
use crate::runner::async_main;

/// TCPKali2 main function
///
/// # Returns
/// * `Result<(), TcpKaliError>` - Execution result
fn main() -> Result<(), TcpKaliError> {
    let matches = new_command();
    let workers = *matches.get_one::<usize>("workers").unwrap();
    if workers == 0 {
        return Err(TcpKaliError::Config(
            "workers must be greater than 0".into(),
        ));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;

    rt.block_on(async_main(matches))
}
