//! Native messaging host for the Downpour Firefox extension.
//!
//! Firefox starts this program with its manifest path and the caller's
//! extension id as arguments and talks to it over stdin/stdout. Stdout
//! carries protocol frames only, so all logging goes to stderr, which
//! Firefox captures in its own log.

mod registration;
mod relay;

use std::io::{self, BufWriter};
use std::process::ExitCode;

use downpour_protocol::messages::{ExtensionRequest, HostReply};
use downpour_protocol::native::{read_message, write_message, NativeMessageError};
use registration::Confirmation;

enum Command {
    /// Started by Firefox (any arguments it passes are informational).
    Relay,
    Install(Confirmation),
    Uninstall(Confirmation),
}

fn parse_command() -> Command {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let confirmation = if args.iter().any(|arg| arg == "--yes") {
        Confirmation::Given
    } else {
        Confirmation::Ask
    };
    match args.first().map(String::as_str) {
        Some("install") => Command::Install(confirmation),
        Some("uninstall") => Command::Uninstall(confirmation),
        _ => Command::Relay,
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(false)
        .init();
    let result = match parse_command() {
        Command::Relay => relay_session().map_err(|error| error.to_string()),
        Command::Install(confirmation) => {
            registration::install(confirmation).map_err(|error| error.to_string())
        }
        Command::Uninstall(confirmation) => {
            registration::uninstall(confirmation).map_err(|error| error.to_string())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("downpour-host: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Serves requests until the browser closes the port. A frame that is not a
/// valid request gets an error reply; broken framing ends the session because
/// the stream position can no longer be trusted.
fn relay_session() -> Result<(), NativeMessageError> {
    let mut stdin = io::stdin().lock();
    let mut stdout = BufWriter::new(io::stdout().lock());
    loop {
        let reply = match read_message::<ExtensionRequest>(&mut stdin) {
            Ok(Some(request)) => relay::handle(request),
            Ok(None) => return Ok(()),
            Err(NativeMessageError::Json(error)) => HostReply::Error {
                message: format!("unsupported request: {error}"),
            },
            Err(error) => return Err(error),
        };
        write_message(&mut stdout, &reply)?;
    }
}
