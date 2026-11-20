mod control;

use std::{io, process::ExitCode};

fn main() -> ExitCode {
    match control::run(&mut io::stdin().lock(), &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shagadelic: {error}");
            ExitCode::FAILURE
        }
    }
}
