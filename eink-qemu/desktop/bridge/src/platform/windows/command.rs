//! Fixed system tools for native bridge operations. No IPC request can supply
//! an executable, script, environment, or working directory to this module.
use crate::model::BridgeError;
use std::{ffi::{OsStr, OsString}, io::Read, os::windows::{ffi::OsStringExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}, process::CommandExt},
    path::PathBuf, process::{Child, Command, Stdio}, ptr, sync::mpsc, thread, time::{Duration, Instant}};
use windows_sys::Win32::{Foundation::*, System::{Diagnostics::ToolHelp::*, JobObjects::*, SystemInformation::*, Threading::*}};
const MAX_OUTPUT: usize = 1024 * 1024;
#[derive(Clone, Copy)]
pub enum Tool { Netsh, PowerShell }
#[derive(Debug)]
pub struct Output { pub code: i32, pub stdout: Vec<u8>, pub stderr: Vec<u8> }
impl Output {
    pub fn checked(self) -> Result<Self, BridgeError> {
        if self.code == 0 { Ok(self) } else { Err(failed("EHOSTCOMMAND", "Windows could not complete the bridge operation.", &self)) }
    }
}
fn error(stage: &str) -> BridgeError { BridgeError::new("EHOSTCOMMAND", format!("{stage}: {}", std::io::Error::last_os_error())) }
fn failed(code: &str, message: &str, output: &Output) -> BridgeError {
    BridgeError { code: code.into(), message: message.into(), detail: Some(format!("exit={}\nstdout:\n{}\nstderr:\n{}", output.code,
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))) }
}
fn handle(value: HANDLE) -> Result<OwnedHandle, BridgeError> {
    if value.is_null() || value == INVALID_HANDLE_VALUE { Err(error("Cannot open Windows process control")) }
    else { Ok(unsafe { OwnedHandle::from_raw_handle(value) }) }
}
fn directory(system: bool) -> Result<PathBuf, BridgeError> {
    let mut buffer = [0u16; 32768];
    let length = unsafe { if system { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) }
        else { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } } as usize;
    if length == 0 || length >= buffer.len() { return Err(error("Cannot locate Windows system tools")); }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}
struct Running { child: Child, job: Option<OwnedHandle> }
impl Drop for Running {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE includes descendants. Also kill the suspended
        // direct child if assignment failed before it entered the job.
        drop(self.job.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn resume(child: &Child) -> Result<(), BridgeError> {
    let snapshot = handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 { return Err(error("Cannot inspect the suspended Windows tool")); }
    let mut first = None;
    loop {
        if entry.th32OwnerProcessID == child.id() {
            if first.is_some() { return Err(BridgeError::new("EHOSTCOMMAND", "The Windows tool started before its process guard was ready.")); }
            first = Some(entry.th32ThreadID);
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            if unsafe { GetLastError() } != ERROR_NO_MORE_FILES { return Err(error("Cannot inspect Windows tool threads")); }
            break;
        }
    }
    let tid = first.ok_or_else(|| BridgeError::new("EHOSTCOMMAND", "The suspended Windows tool exited before startup."))?;
    let thread = handle(unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION, 0, tid) })?;
    if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != child.id() { return Err(BridgeError::new("EHOSTCOMMAND", "Windows tool thread identity changed.")); }
    if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 { return Err(error("Cannot start the guarded Windows tool")); }
    Ok(())
}
fn reader(mut pipe: impl Read + Send + 'static, sender: mpsc::Sender<(bool, std::io::Result<Vec<u8>>)>, stderr: bool) -> Result<(), BridgeError> {
    thread::Builder::new().name("bridge-tool-output".into()).spawn(move || {
        let mut output = Vec::new();
        let result = pipe.by_ref().take(MAX_OUTPUT as u64 + 1).read_to_end(&mut output).map(|_| output);
        let _ = sender.send((stderr, result));
    }).map_err(|e| BridgeError::new("EHOSTCOMMAND", format!("Cannot read Windows tool output: {e}")))?;
    Ok(())
}
/// Arguments are constructed by native platform code only. Network identity
/// validation belongs to that caller; localized output is diagnostic text.
/// PowerShell scripts must use direct process creation (not WMI process launch)
/// so every command remains in this job until the invocation finishes.
pub fn run(tool: Tool, arguments: &[&OsStr], timeout: Duration) -> Result<Output, BridgeError> {
    if timeout.is_zero() || timeout > Duration::from_secs(180) {
        return Err(BridgeError::new("EINVAL", "Invalid Windows bridge operation timeout."));
    }
    let system = directory(true)?; let windows = directory(false)?;
    let powershell = system.join("WindowsPowerShell/v1.0");
    let executable = match tool { Tool::Netsh => system.join("netsh.exe"), Tool::PowerShell => powershell.join("powershell.exe") };
    let job = handle(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe { SetInformationJobObject(job.as_raw_handle(), JobObjectExtendedLimitInformation,
        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of_val(&limits) as u32) } == 0 {
        return Err(error("Cannot contain Windows bridge commands"));
    }
    let child = Command::new(executable).args(arguments).current_dir(&system)
        .env_clear().env("SystemRoot", &windows).env("WINDIR", &windows)
        .env("PATH", &system).env("PSModulePath", powershell.join("Modules"))
        .env("TEMP", windows.join("Temp")).env("TMP", windows.join("Temp"))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED).spawn()
        .map_err(|e| BridgeError::new("EHOSTCOMMAND", format!("Cannot start Windows bridge tool: {e}")))?;
    let mut running = Running { child, job: Some(job) };
    if unsafe { AssignProcessToJobObject(running.job.as_ref().unwrap().as_raw_handle(), running.child.as_raw_handle()) } == 0 {
        return Err(error("Cannot contain the Windows bridge tool"));
    }
    let (sender, receiver) = mpsc::channel();
    reader(running.child.stdout.take().unwrap(), sender.clone(), false)?;
    reader(running.child.stderr.take().unwrap(), sender, true)?;
    let deadline = Instant::now() + timeout;
    resume(&running.child)?;
    let mut output = Output { code: -1, stdout: Vec::new(), stderr: Vec::new() };
    let mut received = 0;
    let mut interrupted = None;
    loop {
        while let Ok((stderr, bytes)) = receiver.try_recv() {
            received += 1;
            let bytes = bytes.map_err(|e| BridgeError::new("EHOSTCOMMAND", format!("Cannot capture Windows tool output: {e}")))?;
            if bytes.len() > MAX_OUTPUT { interrupted = Some("EOUTPUTLIMIT"); }
            if stderr { output.stderr = bytes; } else { output.stdout = bytes; }
        }
        if interrupted.is_some() { break; }
        match running.child.try_wait().map_err(|e| BridgeError::new("EHOSTCOMMAND", e.to_string()))? {
            Some(status) => { output.code = status.code().unwrap_or(-1); break; },
            None if Instant::now() >= deadline => { interrupted = Some("ETIMEDOUT"); break; },
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
    // Even a successful main process cannot leave descendants holding output
    // pipes open or continuing a mutation after the caller starts verification.
    drop(running);
    while received < 2 {
        let (stderr, bytes) = receiver.recv_timeout(Duration::from_secs(5))
            .map_err(|_| BridgeError::new("EHOSTCOMMAND", "Windows tool output did not close after termination."))?;
        received += 1;
        let bytes = bytes.map_err(|e| BridgeError::new("EHOSTCOMMAND", e.to_string()))?;
        if bytes.len() > MAX_OUTPUT { interrupted = Some("EOUTPUTLIMIT"); }
        if stderr { output.stderr = bytes; } else { output.stdout = bytes; }
    }
    match interrupted {
        Some("ETIMEDOUT") => Err(failed("ETIMEDOUT", "The Windows bridge operation timed out. Check recovery before retrying.", &output)),
        Some(_) => Err(failed("EOUTPUTLIMIT", "The Windows bridge tool returned too much output.", &output)),
        None => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn powershell(script: &str, seconds: u64) -> Result<Output, BridgeError> {
        run(Tool::PowerShell, &[OsStr::new("-NoProfile"), OsStr::new("-NonInteractive"), OsStr::new("-Command"), OsStr::new(script)], Duration::from_secs(seconds))
    }
    #[test]
    fn fixed_system_tool_captures_output_and_nonzero_status() {
        let output = powershell("[Console]::Out.Write('bridge-output'); [Console]::Error.Write('bridge-error'); exit 7", 120).unwrap();
        assert_eq!(output.stdout, b"bridge-output"); assert_eq!(output.stderr, b"bridge-error");
        assert_eq!(output.code, 7); assert_eq!(output.checked().unwrap_err().code, "EHOSTCOMMAND");
    }
    #[test]
    fn excessive_output_is_bounded() {
        let error = powershell("[Console]::Out.Write(('x' * 1100000))", 120).unwrap_err();
        assert_eq!(error.code, "EOUTPUTLIMIT"); assert!(error.detail.unwrap().len() < MAX_OUTPUT * 2 + 1024);
    }
    #[test]
    #[ignore = "takes a minute; native Windows timeout and descendant termination test"]
    fn timeout_terminates_the_entire_command_tree() {
        let script = "$p=New-Object Diagnostics.Process; $p.StartInfo.FileName=([Environment]::SystemDirectory+'\\WindowsPowerShell\\v1.0\\powershell.exe'); $p.StartInfo.Arguments='-NoProfile -NonInteractive -Command Start-Sleep -Seconds 120'; $p.StartInfo.UseShellExecute=$false; $p.Start() | Out-Null; [Console]::WriteLine('descendant='+$p.Id); Start-Sleep -Seconds 120";
        let error = powershell(script, 60).unwrap_err();
        assert_eq!(error.code, "ETIMEDOUT");
        let detail = error.detail.unwrap();
        let pid: u32 = detail.lines().find_map(|line| line.strip_prefix("descendant=")).expect(&detail).trim().parse().unwrap();
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() { assert_eq!(unsafe { GetLastError() }, ERROR_INVALID_PARAMETER); }
        else { let process = handle(process).unwrap(); assert_eq!(unsafe { WaitForSingleObject(process.as_raw_handle(), 1000) }, WAIT_OBJECT_0); }
    }
}
