
use std::{fs, process, env, path::PathBuf};

use clap::Parser;

#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(required = true)]
    files: Vec<PathBuf>,

    #[arg(short, long)]
    lines_only: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {

    let cli = Cli::parse();

    for path in &cli.files {
        let text = fs::read_to_string(path)?;
        let lines = text.lines().count();
        
        if cli.lines_only {
            println!("{lines:>8} {}", path.display());
        } else {
            let words = text.split_whitespace().count();
            println!("{lines:>8} {words:>8} {}", path.display());
        }
    }
    Ok(())
}
