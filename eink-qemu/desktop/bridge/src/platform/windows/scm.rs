//! SCM lifecycle for the dedicated LocalSystem helper process. A control
//! callback only requests shutdown; the server drains work and reconciles its
//! journal before this adapter reports SERVICE_STOPPED.
use crate::{model::BridgeError, windows_security};
use std::{ffi::c_void, panic::{catch_unwind, AssertUnwindSafe}, ptr,
    sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}}};
use windows_sys::Win32::{Foundation::*, System::Services::*};

pub const SERVICE_NAME: &str = "eink-qemu-bridge";
type Work = Box<dyn FnOnce(Arc<AtomicBool>, Ready) -> Result<(), BridgeError> + Send>;
struct State { handle: usize, status: SERVICE_STATUS }
struct Context {
    state: Mutex<State>, stop: Arc<AtomicBool>, work: Mutex<Option<Work>>,
    result: Mutex<Option<Result<(), BridgeError>>>,
}
// SCM can deliver callbacks until the dispatcher returns. Process-lifetime
// storage prevents a late callback from accessing a freed context.
static CONTEXT: OnceLock<Context> = OnceLock::new();
fn error(message: &str) -> BridgeError { BridgeError::new("ESERVICE", message) }
fn native_error(message: &str) -> BridgeError {
    error(&format!("{message} ({})", unsafe { GetLastError() }))
}
fn status(state: u32, failed: bool) -> SERVICE_STATUS {
    let pending = matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING);
    SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS, dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN } else { 0 },
        dwWin32ExitCode: if failed { ERROR_SERVICE_SPECIFIC_ERROR } else { 0 },
        dwServiceSpecificExitCode: u32::from(failed), dwCheckPoint: u32::from(pending),
        // Recovery may include bounded PnP and network commands. Do not fake
        // progress by advancing checkpoints while one of those commands waits.
        dwWaitHint: if pending { 30 * 60 * 1000 } else { 0 },
    }
}
impl Context {
    fn report(&self, next: u32, failed: bool) -> Result<(), BridgeError> {
        let mut state = self.state.lock().map_err(|_| error("Service state is unavailable."))?;
        if state.status.dwCurrentState == SERVICE_STOPPED { return Err(error("The service already stopped.")); }
        if next == SERVICE_RUNNING && (state.status.dwCurrentState != SERVICE_START_PENDING || self.stop.load(Ordering::Acquire)) {
            return Err(error("Service startup was interrupted."));
        }
        let updated = status(next, failed);
        if unsafe { SetServiceStatus(state.handle as SERVICE_STATUS_HANDLE, &updated) } == 0 {
            return Err(native_error("Windows could not update bridge service status"));
        }
        state.status = updated;
        Ok(())
    }
}
/// Only the server calls this, after it has opened the protected journal,
/// performed startup recovery, and bound its authenticated endpoint.
pub struct Ready(&'static Context);
impl Ready {
    pub fn running(self) -> Result<(), BridgeError> { self.0.report(SERVICE_RUNNING, false) }
}
unsafe extern "system" fn control(code: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    let Some(context) = CONTEXT.get() else { return ERROR_SERVICE_NOT_ACTIVE; };
    match code {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            context.stop.store(true, Ordering::Release);
            match context.report(SERVICE_STOP_PENDING, false) { Ok(()) => NO_ERROR, Err(_) => ERROR_SERVICE_NOT_ACTIVE }
        },
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    let Some(context) = CONTEXT.get() else { return; };
    let name: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), ptr::null()) };
    if handle.is_null() {
        if let Ok(mut result) = context.result.lock() { *result = Some(Err(native_error("Windows could not register bridge service controls"))); }
        return;
    }
    if let Ok(mut state) = context.state.lock() { state.handle = handle as usize; }
    let result = catch_unwind(AssertUnwindSafe(|| {
        context.report(SERVICE_START_PENDING, false)?;
        let work = context.work.lock().map_err(|_| error("Service startup is unavailable."))?.take()
            .ok_or_else(|| error("Service startup already ran."))?;
        work(context.stop.clone(), Ready(context))
    })).unwrap_or_else(|_| Err(error("The bridge service failed unexpectedly; its journal requires recovery.")));
    let failed = result.is_err();
    if let Ok(mut destination) = context.result.lock() { *destination = Some(result); }
    // Do not touch the status handle after reporting STOPPED: SCM may release
    // the process immediately. All native cleanup belongs in work above.
    let _ = context.report(SERVICE_STOPPED, failed);
}
/// Called once from the helper's main thread. Work and paths are supplied by
/// compiled service code, never by SCM arguments or an IPC request.
pub fn dispatch(work: impl FnOnce(Arc<AtomicBool>, Ready) -> Result<(), BridgeError> + Send + 'static) -> Result<(), BridgeError> {
    windows_security::require_system()?;
    CONTEXT.set(Context { state: Mutex::new(State { handle: 0, status: status(SERVICE_START_PENDING, false) }),
        stop: Arc::new(AtomicBool::new(false)), work: Mutex::new(Some(Box::new(work))), result: Mutex::new(None) })
        .map_err(|_| error("The bridge service dispatcher already ran."))?;
    let mut name: Vec<u16> = SERVICE_NAME.encode_utf16().chain(Some(0)).collect();
    let table = [SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) }, SERVICE_TABLE_ENTRYW::default()];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(native_error("Start bridge support through the Windows Service Control Manager"));
    }
    CONTEXT.get().unwrap().result.lock().map_err(|_| error("Service result is unavailable."))?.take()
        .ok_or_else(|| error("Windows did not start the bridge service"))?
}
