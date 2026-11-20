// The supervisor needs a native child. Reuse the same protocol peer on Windows and Linux.
fn main() -> std::process::ExitCode {
    let status = std::process::Command::new(env!("TJ_FIXTURE_PYTHON"))
        .arg(env!("TJ_FIXTURE_CODEX"))
        .args(std::env::args_os().skip(1))
        .status()
        .expect("run the native Codex fixture");
    std::process::ExitCode::from(status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1))
}
