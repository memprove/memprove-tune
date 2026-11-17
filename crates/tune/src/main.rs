//! `tune`: A/B experiments on the configuration of a coding agent, judged
//! by the repository's own tests.

#![forbid(unsafe_code)]

mod apply;
mod cli;
mod error;
mod experiment;
mod files;
mod ledger;
mod markdown;
mod patch;
mod prune;
mod report;
mod scan;
mod smells;
mod stats;
mod tokens;
mod units;
mod variant;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("tune: cannot tell the current directory: {error}");
            return ExitCode::from(1);
        }
    };
    match cli::run(&args, &cwd, &mut std::io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error::Error::Usage(detail)) => {
            eprintln!("tune: {detail}\nTry `tune --help`.");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("tune: {error}");
            ExitCode::from(1)
        }
    }
}
