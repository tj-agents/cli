use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

struct AttributeList(Vec<usize>);

impl AttributeList {
    fn new(count: u32) -> io::Result<Self> {
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

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "process argument contains a null character"));
    }
    value.push(0);
    Ok(value)
}

fn command_line(arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let mut command = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            command.push(u16::from(b' '));
        }
        command.push(u16::from(b'"'));
        let mut backslashes = 0;
        for character in wide(argument)?.into_iter().take(argument.encode_wide().count()) {
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
        command.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
        command.push(u16::from(b'"'));
    }
    command.push(0);
    Ok(command)
}

fn inherited_handle(identifier: u32) -> io::Result<Option<OwnedHandle>> {
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

fn raw(handle: &Option<OwnedHandle>) -> HANDLE {
    handle.as_ref().map_or(null_mut(), AsRawHandle::as_raw_handle)
}

pub(super) fn run(arguments: &[OsString]) -> io::Result<u32> {
    let executable = which::which(&arguments[0]).map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
    if executable
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "batch files require an explicit cmd.exe command"));
    }
    let executable = wide(executable.as_os_str())?;
    let mut command = command_line(arguments)?;
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
        if WaitForSingleObject(child.as_raw_handle(), INFINITE) == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mut code = 127;
        if GetExitCodeProcess(child.as_raw_handle(), &mut code) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code)
    }
}
