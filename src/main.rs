use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::Result;

use claudegrep::cli::Cli;

fn main() -> ExitCode {
    match claudegrep::app::run(Cli::parse_arguments()).and_then(|output| print(&output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("claudegrep: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Writes `text` to stdout. A closed pipe (`claudegrep … | head`) is not an error.
fn print(text: &str) -> Result<()> {
    match io::stdout().lock().write_all(text.as_bytes()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}
