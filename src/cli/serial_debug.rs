use super::{Console, ErrorCode};

pub(super) use shellrs_cli::Command;

pub(super) fn run(command: Command, console: &mut Console) -> i32 {
    match shellrs_cli::run(
        command,
        &crate::app::data_dir(),
        console.stdout,
        console.stderr,
    ) {
        Ok(code) => code,
        Err(error) => console.error(ErrorCode::ConnectFailed, &error.to_string()),
    }
}
