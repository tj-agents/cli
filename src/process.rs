use std::ffi::OsString;
use std::io;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

pub fn run(arguments: &[OsString]) -> io::Result<u32> {
    if arguments.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: tj_hook -- <executable> [arguments...]"));
    }
    #[cfg(windows)]
    {
        windows::run(arguments)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(std::process::Command::new(&arguments[0]).args(&arguments[1..]).exec())
    }
}
