//! Local named-pipe transport. The pipe authenticates identity; it does not
//! authorize network changes. Clients never receive service process handles.
use super::{Caller, Channel};
use crate::{model::BridgeError, windows_security};
use std::{cell::Cell, fs::File, io::{self, Read, Write}, os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr, time::{Duration, Instant}};
use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*}, Storage::FileSystem::*,
    System::{IO::*, Pipes::*, Threading::*}};

pub const ENDPOINT: &str = r"\\.\pipe\net.serialx.eink-qemu.bridge.v1";
// Excludes FILE_APPEND_DATA, which also means FILE_CREATE_PIPE_INSTANCE.
const CLIENT_ACCESS: u32 = FILE_GENERIC_READ | FILE_WRITE_DATA;
fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
fn error(error: impl std::fmt::Display) -> BridgeError { BridgeError::new("EIPC", error.to_string()) }
fn last() -> io::Error { io::Error::last_os_error() }
fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE { Err(last()) }
    else { Ok(unsafe { OwnedHandle::from_raw_handle(handle) }) }
}

/// Only one I/O operation runs on a Pipe at a time. Each uses a fresh event and
/// OVERLAPPED, and cancellation is drained before its memory can go out of scope.
pub struct Pipe { file: File, deadline: Cell<Instant> }
impl Pipe {
    fn new(handle: HANDLE) -> io::Result<Self> {
        Ok(Self { file: File::from(owned(handle)?), deadline: Cell::new(Instant::now() + Duration::from_secs(5)) })
    }
    pub fn timeout(&self, duration: Duration) { self.deadline.set(Instant::now() + duration); }
    fn overlapped(&self, action: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32, connect: bool) -> io::Result<u32> {
        let event = owned(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })?;
        let mut operation: OVERLAPPED = unsafe { std::mem::zeroed() };
        operation.hEvent = event.as_raw_handle();
        let mut bytes = 0;
        if action(&mut operation, &mut bytes) != 0 { return Ok(bytes); }
        let failure = last();
        if connect && failure.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) { return Ok(0); }
        if failure.raw_os_error() != Some(ERROR_IO_PENDING as i32) { return Err(failure); }
        let milliseconds = self.deadline.get().saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128 - 1) as u32;
        let result = unsafe { WaitForSingleObject(event.as_raw_handle(), milliseconds) };
        if result == WAIT_OBJECT_0 {
            if unsafe { GetOverlappedResult(self.file.as_raw_handle(), &operation, &mut bytes, 0) } == 0 { return Err(last()); }
            return Ok(bytes);
        }
        // CancelIoEx does not wait. The kernel can still access the buffer and
        // OVERLAPPED until GetOverlappedResult reports completion.
        unsafe {
            CancelIoEx(self.file.as_raw_handle(), &operation);
            GetOverlappedResult(self.file.as_raw_handle(), &operation, &mut bytes, 1);
        }
        Err(io::Error::new(if result == WAIT_TIMEOUT { io::ErrorKind::TimedOut } else { io::ErrorKind::Other }, "Bridge pipe operation did not complete."))
    }
    fn wait_client(&self) -> io::Result<()> {
        self.overlapped(|operation, _| unsafe { ConnectNamedPipe(self.file.as_raw_handle(), operation) }, true).map(|_| ())
    }
    /// Call after reading the request: Windows identifies the token associated
    /// with the last message read, not a caller-supplied SID or PID.
    pub fn peer(&self) -> Result<Peer, BridgeError> {
        if unsafe { ImpersonateNamedPipeClient(self.file.as_raw_handle()) } == 0 { return Err(error(last())); }
        struct Revert;
        impl Drop for Revert { fn drop(&mut self) { if unsafe { RevertToSelf() } == 0 { std::process::abort(); } } }
        let revert = Revert;
        let mut token = ptr::null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 { return Err(error(last())); }
        let token = owned(token).map_err(error)?;
        let sid = windows_security::token_sid(token.as_raw_handle())?;
        drop(revert);
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(self.file.as_raw_handle(), &mut pid) } == 0 { return Err(error(last())); }
        let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_DUP_HANDLE | SYNCHRONIZE, 0, pid) }).map_err(error)?;
        let mut process_token = ptr::null_mut();
        if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut process_token) } == 0 { return Err(error(last())); }
        let process_token = owned(process_token).map_err(error)?;
        if windows_security::token_sid(process_token.as_raw_handle())? != sid {
            return Err(BridgeError::new("EPERM", "The bridge caller's process and pipe identities differ."));
        }
        let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        if unsafe { GetProcessTimes(process.as_raw_handle(), &mut times[0], &mut times[1], &mut times[2], &mut times[3]) } == 0 { return Err(error(last())); }
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return Err(BridgeError::new("EPERM", "The bridge caller has exited."));
        }
        Ok(Peer { caller: Caller { principal: format!("sid:{sid}") }, process, pid,
            creation_time: (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime) })
    }
    pub fn send_lease(&mut self, peer: &Peer, lease: &File) -> Result<(), BridgeError> {
        let mut duplicate = ptr::null_mut();
        if unsafe { DuplicateHandle(GetCurrentProcess(), lease.as_raw_handle(), peer.process.as_raw_handle(),
            &mut duplicate, FILE_GENERIC_READ, 0, 0) } == 0 { return Err(error(last())); }
        // Never close a numeric remote handle after publishing it: the client
        // could already have closed/reused it. A lost reply retains this empty,
        // read-only lease until the client exits, safely blocking removal.
        self.write_all(&(duplicate as usize as u64).to_le_bytes()).map_err(error)
    }
    pub fn receive_lease(&mut self) -> Result<File, BridgeError> {
        let mut bytes = [0; 8]; self.read_exact(&mut bytes).map_err(error)?;
        let value = u64::from_le_bytes(bytes);
        if value == 0 || value > isize::MAX as u64 { return Err(error("Invalid bridge lease handle.")); }
        let file = File::from(owned(value as usize as HANDLE).map_err(error)?);
        windows_security::validate_handle(&file, None)?;
        let metadata = file.metadata().map_err(error)?;
        if !metadata.is_file() || metadata.len() != 0 { return Err(error("Invalid bridge lease object.")); }
        Ok(file)
    }
    pub fn acknowledge(&mut self) -> Result<(), BridgeError> {
        self.write_all(&[0xa5]).map_err(error)
    }
    /// Closing a Windows server pipe can discard unread output. Wait for a
    /// bounded acknowledgement instead of unbounded FlushFileBuffers.
    pub fn finish_response(&mut self) -> Result<(), BridgeError> {
        self.timeout(Duration::from_secs(5));
        let mut ack = [0]; self.read_exact(&mut ack).map_err(error)?;
        if ack != [0xa5] { return Err(error("Invalid bridge response acknowledgement.")); }
        Ok(())
    }
}
impl Read for Pipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        if Instant::now() >= self.deadline.get() { return Err(io::ErrorKind::TimedOut.into()); }
        self.overlapped(|operation, bytes| unsafe { ReadFile(self.file.as_raw_handle(), buffer.as_mut_ptr(),
            buffer.len().min(u32::MAX as usize) as u32, bytes, operation) }, false).map(|n| n as usize)
    }
}
impl Write for Pipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        if Instant::now() >= self.deadline.get() { return Err(io::ErrorKind::TimedOut.into()); }
        self.overlapped(|operation, bytes| unsafe { WriteFile(self.file.as_raw_handle(), buffer.as_ptr(),
            buffer.len().min(u32::MAX as usize) as u32, bytes, operation) }, false).map(|n| n as usize)
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) } // FlushFileBuffers waits for the peer and is unbounded.
}
impl Channel for Pipe {
    fn response_timeout(&self, timeout: Duration) -> io::Result<()> { self.timeout(timeout); Ok(()) }
    fn acknowledge(&mut self) -> Result<(), BridgeError> { Pipe::acknowledge(self) }
}
pub struct Peer { caller: Caller, process: OwnedHandle, pub pid: u32, pub creation_time: u64 }
impl Peer { pub fn caller(&self) -> &Caller { &self.caller } }

fn connect_path(path: &str) -> Result<Pipe, BridgeError> {
    let name = wide(path);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let handle = unsafe { CreateFileW(name.as_ptr(), CLIENT_ACCESS, 0, ptr::null(), OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, ptr::null_mut()) };
        if handle != INVALID_HANDLE_VALUE { return Pipe::new(handle).map_err(error); }
        let failure = last();
        if failure.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
            return Err(BridgeError::new("ENEEDSSETUP", "Install or start Windows bridge support."));
        }
        if failure.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || Instant::now() >= deadline { return Err(error(failure)); }
        unsafe { WaitNamedPipeW(name.as_ptr(), 100); }
    }
}
pub fn connect() -> Result<Pipe, BridgeError> {
    let pipe = connect_path(ENDPOINT)?;
    windows_security::validate_pipe(&pipe.file)?;
    Ok(pipe)
}

pub struct Listener { name: String, pending: Pipe }
impl Listener {
    pub fn bind() -> Result<Self, BridgeError> {
        windows_security::require_system()?;
        Self::bind_path(ENDPOINT)
    }
    fn bind_path(name: &str) -> Result<Self, BridgeError> {
        Ok(Self { name: name.into(), pending: Self::instance(name, true)? })
    }
    fn instance(name: &str, first: bool) -> Result<Pipe, BridgeError> {
        let mut descriptor = ptr::null_mut();
        // LocalSystem's default token owner may be Administrators. Specify the
        // service owner explicitly so ordinary clients can authenticate it.
        #[cfg(not(test))]
        let owner = "SY";
        #[cfg(test)]
        let owner = windows_security::current_sid()?;
        let sddl = format!("O:{owner}D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x{CLIENT_ACCESS:x};;;AU)");
        // Unit tests also run as ordinary developers on private, random names.
        // Production bind requires SYSTEM and never adds a caller-owned grant.
        #[cfg(test)]
        let sddl = format!("{sddl}(A;;FA;;;{})", windows_security::current_sid()?);
        let sddl = wide(&sddl);
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, ptr::null_mut()) } == 0 { return Err(error(last())); }
        struct Descriptor(*mut std::ffi::c_void);
        impl Drop for Descriptor { fn drop(&mut self) { unsafe { LocalFree(self.0); } } }
        let _descriptor = Descriptor(descriptor);
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
        let pipe = Pipe::new(unsafe { CreateNamedPipeW(wide(name).as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED
            | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 }, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            32, 65536, 65536, 5000, &attributes) }).map_err(error)?;
        Ok(pipe)
    }
    /// Keeps another instance alive before handing off the connected one, so
    /// the pipe name never becomes available for another server to claim.
    pub fn accept(&mut self, timeout: Duration) -> Result<Option<Pipe>, BridgeError> {
        self.pending.timeout(timeout);
        match self.pending.wait_client() {
            Ok(()) => {
                let next = Self::instance(&self.name, false)?;
                let connected = std::mem::replace(&mut self.pending, next);
                connected.timeout(Duration::from_secs(5));
                Ok(Some(connected))
            },
            Err(failure) if failure.kind() == io::ErrorKind::TimedOut => Ok(None),
            Err(failure) => Err(error(failure)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name() -> String { format!(r"\\.\pipe\eink-bridge-test-{}-{}", std::process::id(), getrandom::u64().unwrap()) }
    #[test]
    fn bounded_pipe_authenticates_kernel_identity_and_keeps_its_name() {
        let name = name();
        let mut listener = Listener::bind_path(&name).unwrap();
        assert!(Listener::bind_path(&name).is_err());
        assert!(listener.accept(Duration::from_millis(20)).unwrap().is_none());
        let worker_name = name.clone();
        let worker = std::thread::spawn(move || {
            let mut pipe = connect_path(&worker_name).unwrap();
            pipe.write_all(b"identity").unwrap();
            let mut response = [0; 2]; pipe.read_exact(&mut response).unwrap();
            assert_eq!(&response, b"ok");
        });
        let mut pipe = listener.accept(Duration::from_secs(5)).unwrap().unwrap();
        let mut message = [0; 8]; pipe.read_exact(&mut message).unwrap();
        assert_eq!(&message, b"identity");
        let peer = pipe.peer().unwrap();
        assert_eq!(peer.caller().principal(), format!("sid:{}", windows_security::current_sid().unwrap()));
        assert_eq!(peer.pid, std::process::id());
        assert!(peer.creation_time > 0);
        pipe.write_all(b"ok").unwrap();
        worker.join().unwrap(); drop(pipe);
        assert!(Listener::bind_path(&name).is_err(), "pending instance must retain the endpoint");
    }
    #[test]
    fn partial_client_times_out_and_does_not_poison_the_listener() {
        let name = name();
        let mut listener = Listener::bind_path(&name).unwrap();
        let mut client = connect_path(&name).unwrap();
        client.write_all(&[0]).unwrap();
        let mut pipe = listener.accept(Duration::from_secs(1)).unwrap().unwrap();
        pipe.timeout(Duration::from_millis(30));
        assert!(super::super::read_request(&mut pipe).is_err());
        drop(pipe); drop(client);
        let _client = connect_path(&name).unwrap();
        assert!(listener.accept(Duration::from_secs(1)).unwrap().is_some());
    }
    #[test]
    #[ignore = "requires LocalSystem in a disposable Windows VM; run with --include-ignored"]
    fn response_and_read_only_lease_survive_the_server_closing() {
        windows_security::require_system().expect("native lease test runs as the disposable VM system test runner");
        let name = name();
        let mut listener = Listener::bind_path(&name).unwrap();
        let root = tempfile::tempdir().unwrap();
        let journal = crate::journal::Journal::open(root.path()).unwrap();
        let id = crate::model::ResourceId::new("12121212121212121212121212121212").unwrap();
        let lease = journal.resource_lock(&id, true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut pipe = listener.accept(Duration::from_secs(5)).unwrap().unwrap();
            let mut request = [0; 1]; pipe.read_exact(&mut request).unwrap();
            let peer = pipe.peer().unwrap();
            pipe.send_lease(&peer, &lease).unwrap();
            drop(lease);
            pipe.finish_response().unwrap();
        });
        let mut client = connect_path(&name).unwrap();
        windows_security::validate_pipe(&client.file).unwrap();
        client.write_all(&[1]).unwrap();
        let mut lease = client.receive_lease().unwrap();
        client.acknowledge().unwrap();
        worker.join().unwrap(); drop(client);
        assert!(lease.write_all(b"must not write").is_err());
        assert_eq!(journal.resource_lock(&id, false).unwrap_err().code, "EBUSY");
        drop(lease);
        assert!(journal.resource_lock(&id, false).is_ok());
    }

    #[test]
    #[ignore = "requires LocalSystem and EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS=1; creates/removes a temporary standard account"]
    fn ordinary_user_cannot_forge_server_and_receives_read_only_lease() {
        const CHILD: &str = "EINK_PIPE_TEST_CHILD";
        if let Ok(name) = std::env::var(CHILD) {
            std::panic::set_hook(Box::new(|failure| {
                let path = format!(r"C:\Windows\Temp\eink-pipe-client-{}.log", std::process::id());
                let _ = std::fs::write(path, failure.to_string());
            }));
            assert_ne!(windows_security::current_sid().unwrap(), "S-1-5-18");
            let mut token = ptr::null_mut();
            assert_ne!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }, 0);
            let token = owned(token).unwrap();
            let mut elevation: TOKEN_ELEVATION = unsafe { std::mem::zeroed() };
            let mut length = 0;
            assert_ne!(unsafe { GetTokenInformation(token.as_raw_handle(), TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(), std::mem::size_of_val(&elevation) as u32, &mut length) }, 0);
            assert_eq!(elevation.TokenIsElevated, 0, "test client must not be elevated");
            // Discovery runs in the ordinary desktop before support is
            // installed; native PnP identity reads must not require elevation.
            crate::inspect().unwrap();
            let mut pipe = connect_path(&name).unwrap();
            pipe.timeout(Duration::from_secs(30));
            windows_security::validate_pipe(&pipe.file).unwrap();
            assert!(Listener::instance(&name, false).is_err(), "ordinary client must not create server instances");
            pipe.write_all(&[1]).unwrap();
            let mut lease = pipe.receive_lease().unwrap();
            assert!(lease.write_all(b"cannot change service lease").is_err());
            pipe.acknowledge().unwrap();
            let mut release = [0]; pipe.read_exact(&mut release).unwrap();
            drop(lease);
            // The test-only bind supports caller ownership; production clients
            // must still reject this otherwise accessible fake service.
            let fake_name = format!("{name}-fake");
            let _fake = Listener::bind_path(&fake_name).unwrap();
            let fake_client = connect_path(&fake_name).unwrap();
            assert_eq!(windows_security::validate_pipe(&fake_client.file).unwrap_err().code, "EPERM");
            return;
        }
        windows_security::require_system().unwrap();
        assert_eq!(std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref(), Ok("1"));
        use windows_sys::Win32::NetworkManagement::NetManagement::*;
        let mut username = wide(&format!("einkQA{:08x}", getrandom::u32().unwrap()));
        let mut password = wide(&format!("aA1!{:016x}{:016x}", getrandom::u64().unwrap(), getrandom::u64().unwrap()));
        let info = USER_INFO_1 { usri1_name: username.as_mut_ptr(), usri1_password: password.as_mut_ptr(),
            usri1_password_age: 0, usri1_priv: USER_PRIV_USER, usri1_home_dir: ptr::null_mut(),
            usri1_comment: ptr::null_mut(), usri1_flags: UF_SCRIPT | UF_NORMAL_ACCOUNT, usri1_script_path: ptr::null_mut() };
        assert_eq!(unsafe { NetUserAdd(ptr::null(), 1, (&info as *const USER_INFO_1).cast(), ptr::null_mut()) }, 0);
        struct Account(Vec<u16>);
        impl Drop for Account { fn drop(&mut self) { unsafe { NetUserDel(ptr::null(), self.0.as_ptr()); } } }
        let account = Account(username);
        let mut token = ptr::null_mut();
        let logged_on = unsafe { LogonUserW(account.0.as_ptr(), wide(".").as_ptr(), password.as_ptr(),
            LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT, &mut token) };
        for character in &mut password { unsafe { ptr::write_volatile(character, 0); } }
        assert_ne!(logged_on, 0, "{}", last());
        let token = owned(token).unwrap();
        let sid = windows_security::token_sid(token.as_raw_handle()).unwrap();
        let name = name();
        let mut listener = Listener::bind_path(&name).unwrap();
        let root = tempfile::tempdir().unwrap();
        let journal = crate::journal::Journal::open(root.path()).unwrap();
        let id = crate::model::ResourceId::new("34343434343434343434343434343434").unwrap();
        let lease = journal.resource_lock(&id, true).unwrap();
        let exe = std::env::current_exe().unwrap();
        let executable = wide(exe.to_str().unwrap());
        let mut command = wide(&format!("\"{}\" --exact transport::windows::tests::ordinary_user_cannot_forge_server_and_receives_read_only_lease --include-ignored", exe.display()));
        let environment = wide(&format!("{CHILD}={name}\0SystemRoot=C:\\Windows\0TEMP=C:\\Windows\\Temp\0"));
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of_val(&startup) as u32;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { CreateProcessAsUserW(token.as_raw_handle(), executable.as_ptr(), command.as_mut_ptr(),
            ptr::null(), ptr::null(), 0, CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            environment.as_ptr().cast(), ptr::null(), &startup, &mut process) }, 0, "{}", last());
        let child = owned(process.hProcess).unwrap();
        let _thread = owned(process.hThread).unwrap();
        struct StopChild(OwnedHandle);
        impl Drop for StopChild {
            fn drop(&mut self) {
                if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } == WAIT_TIMEOUT {
                    unsafe { TerminateProcess(self.0.as_raw_handle(), 1); WaitForSingleObject(self.0.as_raw_handle(), 5000); }
                }
            }
        }
        let child = StopChild(child);
        let diagnostic = format!(r"C:\Windows\Temp\eink-pipe-client-{}.log", process.dwProcessId);
        let mut pipe = listener.accept(Duration::from_secs(30)).unwrap().unwrap_or_else(|| {
            let mut code = 0; unsafe { GetExitCodeProcess(child.0.as_raw_handle(), &mut code); }
            panic!("ordinary user must connect; exit={code}; {}", std::fs::read_to_string(&diagnostic).unwrap_or_default());
        });
        pipe.timeout(Duration::from_secs(30));
        let mut request = [0]; pipe.read_exact(&mut request).unwrap();
        let peer = pipe.peer().unwrap();
        assert_eq!(peer.pid, process.dwProcessId);
        assert_eq!(peer.caller().principal(), format!("sid:{sid}"));
        pipe.send_lease(&peer, &lease).unwrap(); drop(lease);
        pipe.finish_response().unwrap();
        assert_eq!(journal.resource_lock(&id, false).unwrap_err().code, "EBUSY");
        pipe.write_all(&[1]).unwrap();
        assert_eq!(unsafe { WaitForSingleObject(child.0.as_raw_handle(), 30000) }, WAIT_OBJECT_0);
        let mut exit = 1;
        assert_ne!(unsafe { GetExitCodeProcess(child.0.as_raw_handle(), &mut exit) }, 0);
        assert_eq!(exit, 0, "ordinary-user checks failed: {}", std::fs::read_to_string(&diagnostic).unwrap_or_default());
        let _ = std::fs::remove_file(diagnostic);
        assert!(journal.resource_lock(&id, false).is_ok());
        drop(child); drop(token);
        assert_eq!(unsafe { NetUserDel(ptr::null(), account.0.as_ptr()) }, 0, "temporary test account must be removed");
    }
}
