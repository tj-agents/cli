use std::ffi::OsString;
use std::io;
use std::path::Path;

pub mod bounded;

#[allow(unsafe_code)]
pub mod agent;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub struct Background(windows::Background);
#[cfg(unix)]
pub struct Background(std::process::Child);

pub fn background(arguments: &[OsString], cwd: &Path) -> io::Result<Background> {
    #[cfg(windows)]
    {
        windows::background(arguments, cwd).map(Background)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let Some(program) = arguments.first() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no executable given"));
        };
        std::process::Command::new(program)
            .args(&arguments[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(Background)
    }
}

impl Background {
    pub fn running(&mut self) -> io::Result<bool> {
        #[cfg(windows)]
        {
            self.0.running()
        }
        #[cfg(unix)]
        {
            self.0.try_wait().map(|status| status.is_none())
        }
    }

    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.0.kill()
        }
        #[cfg(unix)]
        {
            if let Ok(pid) = i32::try_from(self.0.id())
                && let Some(group) = rustix::process::Pid::from_raw(pid)
            {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = self.0.kill();
            self.0.wait().map(|_| ())
        }
    }
}

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
