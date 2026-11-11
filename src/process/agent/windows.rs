use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, FILETIME, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, GenerateConsoleCtrlEvent, INPUT_RECORD, KEY_EVENT, LEFT_CTRL_PRESSED,
    STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, WriteConsoleInputW,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO, VK_RETURN};

use super::super::windows::{AttributeList, command_line, inherited_handle, is_batch, raw, wide};
use super::Stop;

const STILL_ACTIVE: u32 = 259;

pub struct Agent {
    pid: u32,
    process: OwnedHandle,
    job: OwnedHandle,
    exit: Option<i32>,
}

fn environment_block(additions: &[(&str, String)]) -> Vec<u16> {
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !additions.iter().any(|(added, _)| name.eq_ignore_ascii_case(added)))
        .chain(additions.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))))
        .collect();
    variables.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (name, value) in variables {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

impl Agent {
    pub fn start(argv: &[OsString], cwd: &Path, env: &[(&str, String)]) -> io::Result<Self> {
        let executable = std::path::absolute(
            which::which(&argv[0]).map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?,
        )?;
        if is_batch(&executable) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a batch file; pass the agent's executable", executable.display()),
            ));
        }
        let executable = wide(executable.as_os_str())?;
        let mut command = command_line(argv)?;
        let directory = wide(cwd.as_os_str())?;
        let mut environment = environment_block(env);
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
            handles.dedup();
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
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
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
                CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_mut_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut process,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            drop(OwnedHandle::from_raw_handle(process.hThread));
            Ok(Self {
                pid: process.dwProcessId,
                process: OwnedHandle::from_raw_handle(process.hProcess),
                job,
                exit: None,
            })
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if self.exit.is_none() && unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            let mut code = 0;
            if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.exit = Some(code as i32);
        }
        Ok(self.exit)
    }

    pub fn stop(&mut self, stop: Stop) -> io::Result<()> {
        match stop {
            Stop::Signal => {
                if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.pid) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
            Stop::Keys => press_ctrl_c(),
        }
    }

    pub fn kill_tree(&mut self) -> io::Result<bool> {
        unsafe {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            if QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if accounting.ActiveProcesses == 0 {
                return Ok(true);
            }
            if TerminateJobObject(self.job.as_raw_handle(), 1) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(false)
    }

    pub fn reclaim_terminal(&self) {}
}

fn key(character: u16, virtual_key: u16, scan: u16, control: u32) -> [INPUT_RECORD; 2] {
    let mut records: [INPUT_RECORD; 2] = unsafe { zeroed() };
    for (record, down) in records.iter_mut().zip([1, 0]) {
        record.EventType = KEY_EVENT as u16;
        record.Event.KeyEvent.bKeyDown = down;
        record.Event.KeyEvent.wRepeatCount = 1;
        record.Event.KeyEvent.wVirtualKeyCode = virtual_key;
        record.Event.KeyEvent.wVirtualScanCode = scan;
        record.Event.KeyEvent.uChar.UnicodeChar = character;
        record.Event.KeyEvent.dwControlKeyState = control;
    }
    records
}

fn write_input(records: &[INPUT_RECORD]) -> io::Result<()> {
    let name: Vec<u16> = OsStr::new("CONIN$").encode_wide().chain([0]).collect();
    unsafe {
        let input = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if input == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let input = OwnedHandle::from_raw_handle(input);
        let length = u32::try_from(records.len()).map_err(io::Error::other)?;
        let mut written = 0;
        if WriteConsoleInputW(input.as_raw_handle(), records.as_ptr(), length, &mut written) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn press_ctrl_c() -> io::Result<()> {
    write_input(&key(3, u16::from(b'C'), 0x2e, LEFT_CTRL_PRESSED))
}

pub fn type_text(text: &str) -> io::Result<()> {
    let records: Vec<INPUT_RECORD> = text.encode_utf16().flat_map(|unit| key(unit, 0, 0, 0)).collect();
    write_input(&records)
}

pub fn press_enter() -> io::Result<()> {
    write_input(&key(u16::from(b'\r'), VK_RETURN, 0x1c, 0))
}

pub fn input_idle() -> Option<Duration> {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return None;
    }
    Some(Duration::from_millis(u64::from(unsafe { GetTickCount() }.wrapping_sub(info.dwTime))))
}

pub fn processes() -> io::Result<std::collections::HashMap<u32, (u32, String)>> {
    let mut parents = std::collections::HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = OwnedHandle::from_raw_handle(snapshot);
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot.as_raw_handle(), &mut entry) != 0;
        while more {
            let length = entry.szExeFile.iter().position(|unit| *unit == 0).unwrap_or(entry.szExeFile.len());
            let image = String::from_utf16_lossy(&entry.szExeFile[..length]);
            parents.insert(entry.th32ProcessID, (entry.th32ParentProcessID, image));
            more = Process32NextW(snapshot.as_raw_handle(), &mut entry) != 0;
        }
    }
    Ok(parents)
}

unsafe extern "system" fn swallow(event: u32) -> i32 {
    i32::from(event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
}

pub fn prepare_wrapper() -> io::Result<()> {
    unsafe {
        if SetConsoleCtrlHandler(None, 0) == 0 || SetConsoleCtrlHandler(Some(swallow), 1) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn started_at(pid: u32) -> io::Result<Option<SystemTime>> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) { Ok(None) } else { Err(error) };
        }
        let process = OwnedHandle::from_raw_handle(process);
        let mut code = 0;
        if GetExitCodeProcess(process.as_raw_handle(), &mut code) == 0 {
            return Err(io::Error::last_os_error());
        }
        if code != STILL_ACTIVE {
            return Ok(None);
        }
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
            (zeroed(), zeroed(), zeroed(), zeroed());
        if GetProcessTimes(process.as_raw_handle(), &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return Err(io::Error::last_os_error());
        }
        let ticks = (u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
            .checked_sub(116_444_736_000_000_000)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process creation time"))?;
        Ok(Some(UNIX_EPOCH + Duration::from_nanos(ticks * 100)))
    }
}

pub fn hidden(program: &str, hook: &Path) -> Command {
    let mut command = Command::new(hook);
    command.arg("--").arg(program);
    command
}
