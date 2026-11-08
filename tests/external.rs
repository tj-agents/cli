use std::path::Path;
use std::process::Command;

fn tj(path: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tj")).args(arguments).env("PATH", path).output().unwrap()
}

fn write_tool(directory: &Path) {
    #[cfg(windows)]
    std::fs::write(directory.join("tj-echo.cmd"), "@echo off\r\necho %*\r\nexit /b 7\r\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tool = directory.join("tj-echo");
        std::fs::write(&tool, "#!/bin/sh\necho \"$@\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn unknown_subcommand_runs_the_matching_tool_on_path() {
    let directory = tempfile::tempdir().unwrap();
    write_tool(directory.path());
    let output = tj(directory.path(), &["echo", "progress", "461"]);
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "progress 461");
}

#[test]
fn unknown_subcommand_without_a_tool_names_the_missing_program() {
    let directory = tempfile::tempdir().unwrap();
    let output = tj(directory.path(), &["nothing-here"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no `tj-nothing-here` is on PATH"));
}
