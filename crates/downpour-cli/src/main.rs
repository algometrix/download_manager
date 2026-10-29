//! `downpour-cli`: Downpour from the command line.
//!
//! Exit codes: 0 everything was saved or sent, 1 something failed,
//! 2 the command line was wrong, 130 stopped with Ctrl+C.

mod args;
mod get;
mod progress;
mod send;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use url::Url;

use args::{Cli, Command, Links};

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_INTERRUPTED: u8 = 130;

fn main() -> ExitCode {
    let cli = Cli::parse();
    // The engine's own warnings repeat the results this tool prints, so
    // they only show with --verbose (or RUST_LOG).
    let level = if cli.verbose {
        "downpour_core=info,downpour_cli=info"
    } else {
        "downpour_core=error,downpour_cli=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Get(args) => {
            let Some(urls) = addresses(&args.links, args.file_name.is_some()) else {
                return ExitCode::from(EXIT_USAGE);
            };
            match get::run(args, urls) {
                Ok(get::RunResult::AllSaved) => ExitCode::SUCCESS,
                Ok(get::RunResult::SomeFailed) => ExitCode::from(EXIT_FAILED),
                Ok(get::RunResult::Interrupted) => ExitCode::from(EXIT_INTERRUPTED),
                Err(error) => {
                    eprintln!("downpour-cli: {error}");
                    ExitCode::from(EXIT_FAILED)
                }
            }
        }
        Command::Send(args) => {
            let Some(urls) = addresses(&args.links, args.file_name.is_some()) else {
                return ExitCode::from(EXIT_USAGE);
            };
            match send::run(&args, urls) {
                Ok(count) => {
                    let noun = if count == 1 { "link" } else { "links" };
                    println!("Sent {count} {noun} to Downpour.");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("downpour-cli: {error}");
                    ExitCode::from(EXIT_FAILED)
                }
            }
        }
    }
}

/// The addresses from the arguments and the list file, checked. Prints why
/// and returns `None` when the command line cannot work.
fn addresses(links: &Links, named: bool) -> Option<Vec<Url>> {
    let mut texts: Vec<String> = links.urls.clone();
    if let Some(path) = &links.input {
        let listed = if path.as_os_str() == "-" {
            std::io::read_to_string(std::io::stdin())
        } else {
            std::fs::read_to_string(path)
        };
        match listed {
            Ok(text) => texts.extend(args::addresses_in(&text).map(str::to_owned)),
            Err(error) => {
                eprintln!("downpour-cli: could not read {}: {error}", path.display());
                return None;
            }
        }
    }
    let mut urls = Vec::new();
    let mut bad = false;
    for text in &texts {
        match Url::parse(text) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => urls.push(url),
            Ok(url) => {
                eprintln!(
                    "downpour-cli: {text}: only http and https addresses work, not {}",
                    url.scheme()
                );
                bad = true;
            }
            Err(error) => {
                eprintln!("downpour-cli: {text}: not an address ({error})");
                bad = true;
            }
        }
    }
    if bad {
        return None;
    }
    if urls.is_empty() {
        eprintln!("downpour-cli: give at least one address, or a list with --input");
        return None;
    }
    if named && urls.len() > 1 {
        eprintln!("downpour-cli: --name works with a single address");
        return None;
    }
    Some(urls)
}
