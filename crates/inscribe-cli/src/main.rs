//! Inscribe CLI binary entrypoint.

use clap::Parser;
use inscribe_cli::args::{Cli, Commands};
use inscribe_cli::commands::{run_flash, run_inspect, run_list};
use inscribe_core::drive::default_scanner;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let scanner = default_scanner();

    let result = match &cli.command {
        Commands::List(args) => run_list(args, scanner.as_ref()).map(|out| {
            print!("{out}");
        }),
        Commands::Inspect(args) => run_inspect(args).map(|out| {
            println!("{out}");
        }),
        Commands::Flash(args) => {
            let mut stdin = std::io::stdin().lock();
            let mut stdout = std::io::stdout();
            run_flash(args, &mut stdin, &mut stdout, scanner.as_ref()).map(|_| ())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}
