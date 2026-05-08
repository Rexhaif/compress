mod algorithms;
mod cli;
mod error;
mod io_plan;
mod registry;

use crate::error::Result;

fn main() {
    if let Err(error) = run() {
        let exit_code = error.exit_code();
        if exit_code == 0 {
            println!("{error}");
        } else {
            eprintln!("compress: {error}");
        }

        std::process::exit(exit_code);
    }
}

fn run() -> Result<()> {
    let options = cli::parse(std::env::args())?;

    io_plan::execute(&options)
}
