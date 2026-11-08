use std::ffi::OsString;
use std::io;

#[allow(unsafe_code)]
pub mod agent;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

pub fn run(arguments: &[OsString], preserve_descendants: bool) -> io::Result<u32> {
    if arguments.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: tj_hook -- <executable> [arguments...]"));
    }
    #[cfg(windows)]
    {
        windows::run(arguments, preserve_descendants)
    }
    #[cfg(unix)]
    {
        let _ = preserve_descendants;
        use std::os::unix::process::CommandExt;
        Err(std::process::Command::new(&arguments[0]).args(&arguments[1..]).exec())
    }
}

#[cfg(windows)]
pub fn batch_dispatch(arguments: &[OsString]) -> io::Result<u32> {
    windows::batch_dispatch(arguments)
}
