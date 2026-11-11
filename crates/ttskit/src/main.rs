use clap::Parser;

/// Unofficial command line toolkit for Tabletop Simulator mod development.
#[derive(Parser)]
#[command(version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("ttskit is in early development. Run `ttskit --help` for details.");
}
