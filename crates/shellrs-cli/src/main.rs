use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "shelllf-cli",
    version,
    about = "ShellLF headless LAN serial debugging client"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Lan {
        #[command(subcommand)]
        command: LanCommand,
    },
}

#[derive(Subcommand)]
enum LanCommand {
    Debug {
        #[command(subcommand)]
        command: shellrs_cli::Command,
    },
}

fn main() {
    let Command::Lan {
        command: LanCommand::Debug { command },
    } = Cli::parse().command;
    let directory = std::env::var_os("SHELLRS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| ".".into())
                .join("shellrs")
        });
    let code = match shellrs_cli::run(
        command,
        &directory,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("shelllf-cli: {error}");
            255
        }
    };
    std::process::exit(code);
}
