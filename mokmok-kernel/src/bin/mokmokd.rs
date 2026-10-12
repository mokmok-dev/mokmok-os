use clap::Parser as _;
use mokmok_kernel::{Args, Command, Config};

fn main() -> std::process::ExitCode {
    let args = Args::parse();

    match Config::resolve(&args) {
        Ok(config) => report(&config),
        Err(error) => {
            eprintln!("mokmokd: {error}");
            return std::process::ExitCode::FAILURE;
        },
    }

    match args.command {
        Command::Submit(_)
        | Command::Status { .. }
        | Command::Reconcile { .. }
        | Command::Serve { .. } => {
            eprintln!("mokmokd: not wired yet");
            std::process::ExitCode::FAILURE
        },
    }
}

fn report(config: &Config) {
    println!("data directory: {}", config.data_dir.display());
}
