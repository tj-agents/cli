use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use super::bounded::{CLEANUP_LIMIT, Outcome};

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_CANT_ACCESS_FILE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    AllocConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetExitCodeProcess, GetStartupInfoW, INFINITE, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

pub(super) struct Background(OwnedHandle);

impl Background {
    pub(super) fn running(&self) -> io::Result<bool> {
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        if !self.running()? {
            return Ok(());
        }
        if unsafe { TerminateProcess(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 3_000) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(io::ErrorKind::TimedOut, "background process did not exit")),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

pub(super) fn background(arguments: &[OsString], cwd: &std::path::Path) -> io::Result<Background> {
    let Some(program) = arguments.first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no executable given"));
    };
    if !std::path::Path::new(program).is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "background executable must be absolute"));
    }
    let executable = wide(program)?;
    let mut command = command_line(arguments)?;
    let directory = wide(cwd.as_os_str())?;
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        startup.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
        startup.wShowWindow = 0;
        let mut process: PROCESS_INFORMATION = zeroed();
        if CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_NO_WINDOW,
            null(),
            directory.as_ptr(),
            &startup,
            &mut process,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        drop(OwnedHandle::from_raw_handle(process.hThread));
        Ok(Background(OwnedHandle::from_raw_handle(process.hProcess)))
    }
}

pub(super) struct AttributeList(pub(super) Vec<usize>);

impl AttributeList {
    pub(super) fn new(count: u32) -> io::Result<Self> {
        unsafe {
            let mut size = 0;
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0; size.div_ceil(size_of::<usize>())];
            if InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut size) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(storage))
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast());
        }
    }
}

pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "process argument contains a null character"));
    }
    value.push(0);
    Ok(value)
}

pub(super) fn command_line(arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            command.push(u16::from(b' '));
        }
        let argument = wide(argument)?;
        let argument = &argument[..argument.len() - 1];
        let quoted = argument.is_empty() || argument.contains(&u16::from(b' ')) || argument.contains(&u16::from(b'\t'));
        if quoted {
            command.push(u16::from(b'"'));
        }
        let mut backslashes = 0;
        for &character in argument {
            if character == u16::from(b'\\') {
                backslashes += 1;
                continue;
            }
            if character == u16::from(b'"') {
                backslashes = backslashes * 2 + 1;
            }
            command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
            backslashes = 0;
            command.push(character);
        }
        command.extend(std::iter::repeat_n(u16::from(b'\\'), if quoted { backslashes * 2 } else { backslashes }));
        if quoted {
            command.push(u16::from(b'"'));
        }
    }
    command.push(0);
    Ok(command)
}

fn shell_command_line(arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let Some(mode) = arguments[1..]
        .iter()
        .position(|argument| argument.eq_ignore_ascii_case("/c") || argument.eq_ignore_ascii_case("/k"))
        .map(|offset| offset + 1)
    else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cmd.exe requires a separate /c or /k switch"));
    };
    let options = ["/d", "/s", "/q", "/a", "/u", "/e:on", "/e:off", "/f:on", "/f:off", "/v:on", "/v:off"];
    if arguments[1..mode].iter().any(|argument| !options.iter().any(|option| argument.eq_ignore_ascii_case(option))) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cmd.exe options must be separate supported switches"));
    }
    let (prefix, script) = arguments.split_at(mode + 1);
    if script.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cmd.exe /c and /k require exactly one complete command string",
        ));
    }
    let mut command = command_line(prefix)?;
    command.pop();
    command.extend([u16::from(b' '), u16::from(b'"')]);
    let script = wide(&script[0])?;
    command.extend_from_slice(&script[..script.len() - 1]);
    command.extend([u16::from(b'"'), 0]);
    Ok(command)
}

pub(super) fn inherited_handle(identifier: u32) -> io::Result<Option<OwnedHandle>> {
    unsafe {
        let original = GetStdHandle(identifier);
        if original.is_null() || original == INVALID_HANDLE_VALUE {
            return Ok(None);
        }
        let current = GetCurrentProcess();
        let mut duplicate = null_mut();
        if DuplicateHandle(current, original, current, &mut duplicate, 0, 1, DUPLICATE_SAME_ACCESS) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(OwnedHandle::from_raw_handle(duplicate)))
    }
}

pub(super) fn raw(handle: &Option<OwnedHandle>) -> HANDLE {
    handle.as_ref().map_or(null_mut(), AsRawHandle::as_raw_handle)
}

pub(super) fn is_batch(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"))
}

fn validate_batch(arguments: &[OsString], batch: &std::path::Path) -> io::Result<()> {
    if arguments.iter().any(|argument| argument.to_string_lossy().contains(['\r', '\n', '&', '|', '<', '>', '^', '%']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch arguments contain unsupported shell characters",
        ));
    }
    if !batch.is_absolute() || !is_batch(batch) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch target must be an absolute cmd or bat file"));
    }
    let canonical = batch.canonicalize()?;
    if canonical != batch {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch target must be canonical"));
    }
    if batch.as_os_str().encode_wide().any(|character| character == u16::from(b'%')) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch target path cannot contain percent"));
    }
    let mut sidecar = batch.as_os_str().to_os_string();
    sidecar.push(".exe");
    if std::path::Path::new(&sidecar).is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch target executable sidecar exists"));
    }
    Ok(())
}

fn batch_command(arguments: &[OsString], batch: &std::path::Path) -> io::Result<Vec<OsString>> {
    validate_batch(arguments, batch)?;
    Ok([
        vec![
            std::env::current_exe()?.into_os_string(),
            OsString::from("--tj-hook-batch-dispatch"),
            OsString::from("--"),
            batch.as_os_str().to_os_string(),
        ],
        arguments[1..].to_vec(),
    ]
    .concat())
}

pub(super) fn batch_dispatch(arguments: &[OsString]) -> io::Result<u32> {
    if arguments.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid internal batch dispatch arguments"));
    }
    let batch = std::path::Path::new(&arguments[0]);
    validate_batch(arguments, batch)?;
    unsafe {
        let mut startup = zeroed();
        GetStartupInfoW(&mut startup);
        if startup.dwFlags & STARTF_USESHOWWINDOW == 0 || startup.wShowWindow != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "internal batch dispatch is not hidden"));
        }
        let mut in_job = 0;
        if IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) == 0 || in_job == 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "internal batch dispatch is not job-owned"));
        }
        if AllocConsole() == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let status = Command::new(batch).args(&arguments[1..]).status()?;
    Ok(status.code().unwrap_or(127) as u32)
}

pub(super) fn run(arguments: &[OsString], preserve_descendants: bool) -> io::Result<u32> {
    run_owned(arguments, preserve_descendants, None, || false).map(|outcome| outcome.code())
}

pub(super) fn run_bounded(arguments: &[OsString], lifetime: Duration, cancelled: impl Fn() -> bool) -> Outcome {
    // Errors before the bounded wait are creation failures: no command was started.
    run_owned(arguments, false, Some(lifetime), cancelled).unwrap_or_else(Outcome::Failed)
}

fn settle_job(process: &OwnedHandle, job: &OwnedHandle) -> io::Result<()> {
    let deadline = Instant::now() + CLEANUP_LIMIT;
    unsafe {
        if TerminateJobObject(job.as_raw_handle(), 1) == 0 {
            return Err(io::Error::last_os_error());
        }
        loop {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            if QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if accounting.ActiveProcesses == 0 && WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned job still contains processes after termination",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn bounded_wait(process: &OwnedHandle, job: &OwnedHandle, lifetime: Duration, cancelled: impl Fn() -> bool) -> Outcome {
    let deadline = Instant::now() + lifetime;
    let outcome = loop {
        match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 127;
                break if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                    Outcome::Failed(io::Error::last_os_error())
                } else {
                    Outcome::Exited(code)
                };
            }
            WAIT_TIMEOUT => {}
            _ => break Outcome::Failed(io::Error::last_os_error()),
        }
        if cancelled() {
            break Outcome::Cancelled;
        }
        if Instant::now() >= deadline {
            break Outcome::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    match settle_job(process, job) {
        Ok(()) => outcome,
        Err(error) => Outcome::Unsettled(error),
    }
}

fn run_owned(
    arguments: &[OsString],
    preserve_descendants: bool,
    lifetime: Option<Duration>,
    cancelled: impl Fn() -> bool,
) -> io::Result<Outcome> {
    let executable = std::path::absolute(
        which::which(&arguments[0]).map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?,
    )?;
    let canonical = match executable.canonicalize() {
        Ok(path) => path,
        Err(error) if error.raw_os_error() == Some(ERROR_CANT_ACCESS_FILE as i32) => executable.clone(),
        Err(error) => return Err(error),
    };
    let batch = is_batch(&canonical);
    let broker = batch.then(|| batch_command(arguments, &canonical)).transpose()?;
    let launch_arguments = broker.as_deref().unwrap_or(arguments);
    let mut command = if batch {
        command_line(launch_arguments)?
    } else if canonical.file_name().is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe")) {
        shell_command_line(arguments)?
    } else {
        command_line(launch_arguments)?
    };
    let executable = wide(if batch { launch_arguments[0].as_os_str() } else { executable.as_os_str() })?;
    let stdin = inherited_handle(STD_INPUT_HANDLE)?;
    let stdout = inherited_handle(STD_OUTPUT_HANDLE)?;
    let stderr = inherited_handle(STD_ERROR_HANDLE)?;
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(job);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut jobs = [job.as_raw_handle()];
        let mut handles: Vec<_> =
            [raw(&stdin), raw(&stdout), raw(&stderr)].into_iter().filter(|handle| !handle.is_null()).collect();
        let mut attributes = AttributeList::new(if handles.is_empty() { 1 } else { 2 })?;
        let attribute_list = attributes.0.as_mut_ptr().cast();
        if UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            jobs.as_mut_ptr().cast(),
            size_of_val(&jobs),
            null_mut(),
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if !handles.is_empty()
            && UpdateProcThreadAttribute(
                attribute_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_mut_ptr().cast(),
                size_of_val(handles.as_slice()),
                null_mut(),
                null_mut(),
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
        startup.StartupInfo.wShowWindow = 0;
        startup.StartupInfo.hStdInput = raw(&stdin);
        startup.StartupInfo.hStdOutput = raw(&stdout);
        startup.StartupInfo.hStdError = raw(&stderr);
        startup.lpAttributeList = attribute_list;
        let mut process: PROCESS_INFORMATION = zeroed();
        if CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            i32::from(!handles.is_empty()),
            CREATE_NEW_CONSOLE | EXTENDED_STARTUPINFO_PRESENT,
            null(),
            null(),
            &startup.StartupInfo,
            &mut process,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let child = OwnedHandle::from_raw_handle(process.hProcess);
        let thread = OwnedHandle::from_raw_handle(process.hThread);
        drop(thread);
        if let Some(lifetime) = lifetime {
            return Ok(bounded_wait(&child, &job, lifetime, cancelled));
        }
        if WaitForSingleObject(child.as_raw_handle(), INFINITE) == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut code = 127;
        if GetExitCodeProcess(child.as_raw_handle(), &mut code) == 0 {
            return Err(io::Error::last_os_error());
        }
        if preserve_descendants {
            limits.BasicLimitInformation.LimitFlags = 0;
            if SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Outcome::Exited(code))
    }
}
