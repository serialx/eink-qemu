//! Fixed package-maintenance actions. Only this helper is elevated; neither
//! the desktop nor QEMU runs inside an installer or service token.
use crate::{model::BridgeError,windows_driver::VerifiedPackage,windows_elevation::{self,InstalledHelper},windows_scm,windows_security};
use std::{fs::{File,OpenOptions},os::windows::{ffi::OsStrExt,fs::OpenOptionsExt,io::{AsRawHandle,FromRawHandle,OwnedHandle}},
    path::Path,ptr,time::{Duration,Instant}};
use windows_sys::Win32::{Foundation::*,Security::{*,Authorization::*},Storage::FileSystem::*,System::{Services::*,Threading::*}};

fn error(message:&str)->BridgeError{BridgeError::new("EINSTALL",message)}
fn native(message:&str)->BridgeError{error(&format!("{message} ({})",unsafe{GetLastError()}))}
fn wide(value:&str)->Vec<u16>{value.encode_utf16().chain(Some(0)).collect()}
fn admin()->Result<(),BridgeError>{
    let mut token=ptr::null_mut();
    if unsafe{OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&mut token)}==0{return Err(native("Cannot inspect package-manager privileges"));}
    let token=unsafe{OwnedHandle::from_raw_handle(token)};
    if windows_security::token_sid(token.as_raw_handle())?=="S-1-5-18" || windows_security::token_is_elevated_admin(token.as_raw_handle())?{Ok(())}
    else{Err(BridgeError::new("EPERM","Windows administrator approval is required to maintain bridge support."))}
}
struct Service(SC_HANDLE);
impl Drop for Service{fn drop(&mut self){unsafe{CloseServiceHandle(self.0);}}}
fn manager(create:bool)->Result<Service,BridgeError>{
    let handle=unsafe{OpenSCManagerW(ptr::null(),ptr::null(),SC_MANAGER_CONNECT|if create{SC_MANAGER_CREATE_SERVICE}else{0})};
    if handle.is_null(){Err(native("Cannot open Windows services"))}else{Ok(Service(handle))}
}
fn open(manager:&Service)->Result<Option<Service>,BridgeError>{
    let name=wide(windows_scm::SERVICE_NAME);
    let handle=unsafe{OpenServiceW(manager.0,name.as_ptr(),SERVICE_QUERY_CONFIG|SERVICE_QUERY_STATUS|SERVICE_START|SERVICE_STOP|SERVICE_CHANGE_CONFIG|DELETE|READ_CONTROL)};
    if handle.is_null(){if unsafe{GetLastError()}==ERROR_SERVICE_DOES_NOT_EXIST{Ok(None)}else{Err(native("Cannot open bridge support"))}}
    else{Ok(Some(Service(handle)))}
}
fn command(helper:&InstalledHelper,cleanup:bool)->String{
    // PathBuf::join can preserve a slash in a fixed relative component even
    // on Windows. Store the same canonical separator spelling in SCM that the
    // installer uses; arguments and quoting still have to match exactly.
    format!("\"{}\" {}",helper.path.to_string_lossy().replace('/',"\\"),if cleanup{"--uninstall-service"}else{"--serve"})
}
unsafe fn string(value:*const u16)->Result<String,BridgeError>{
    if value.is_null(){return Err(error("Missing service configuration"));}
    let mut len=0;
    while len<32768 && unsafe{*value.add(len)}!=0{len+=1;}
    if len==32768{return Err(error("Invalid service configuration"));}
    String::from_utf16(unsafe{std::slice::from_raw_parts(value,len)}).map_err(|_|error("Invalid service configuration"))
}
fn validate(service:&Service,helper:&InstalledHelper)->Result<u32,BridgeError>{
    windows_security::validate_service(service.0.cast())?;
    let mut needed=0;
    unsafe{QueryServiceConfigW(service.0,ptr::null_mut(),0,&mut needed);}
    if needed<std::mem::size_of::<QUERY_SERVICE_CONFIGW>() as u32 || needed>65536{return Err(error("Invalid bridge service configuration"));}
    let mut buffer=vec![0u64;(needed as usize).div_ceil(8)];
    let config=buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    if unsafe{QueryServiceConfigW(service.0,config,needed,&mut needed)}==0{return Err(native("Cannot inspect bridge support"));}
    let config=unsafe{&*config};
    let executable=unsafe{string(config.lpBinaryPathName)}?;
    if config.dwServiceType!=SERVICE_WIN32_OWN_PROCESS || unsafe{string(config.lpServiceStartName)}?!="LocalSystem"
        || (executable!=command(helper,false) && executable!=command(helper,true)){
        return Err(error("The existing bridge service differs from this package. Preserve it and review its configuration."));
    }
    Ok(config.dwStartType)
}
fn status(service:&Service)->Result<SERVICE_STATUS_PROCESS,BridgeError>{
    let mut value=SERVICE_STATUS_PROCESS::default();let mut bytes=0;
    if unsafe{QueryServiceStatusEx(service.0,SC_STATUS_PROCESS_INFO,(&mut value as *mut SERVICE_STATUS_PROCESS).cast(),std::mem::size_of_val(&value) as u32,&mut bytes)}==0{return Err(native("Cannot read bridge service status"));}
    Ok(value)
}
fn wait(service:&Service,target:u32)->Result<SERVICE_STATUS_PROCESS,BridgeError>{
    let deadline=Instant::now()+Duration::from_secs(1800);
    loop{
        let value=status(service)?;
        if value.dwCurrentState==target{return Ok(value);}
        if value.dwCurrentState==SERVICE_STOPPED && target==SERVICE_RUNNING{return Err(error("Bridge support failed to start. Keep its package installed and review recovery."));}
        if Instant::now()>=deadline{return Err(error("Bridge support is still working. Keep its package installed and retry maintenance after it finishes."));}
        std::thread::sleep(Duration::from_millis(250));
    }
}
fn stop(service:&Service)->Result<(),BridgeError>{
    let current=status(service)?;
    if current.dwCurrentState==SERVICE_STOPPED{return Ok(());}
    if current.dwCurrentState==SERVICE_START_PENDING{wait(service,SERVICE_RUNNING)?;return stop(service);}
    let handle=unsafe{OpenProcess(SYNCHRONIZE,0,current.dwProcessId)};
    if handle.is_null(){return Err(native("Cannot wait for the bridge helper process"));}
    let process=unsafe{OwnedHandle::from_raw_handle(handle)};
    if current.dwCurrentState!=SERVICE_STOP_PENDING{
        let mut value=SERVICE_STATUS::default();
        if unsafe{ControlService(service.0,SERVICE_CONTROL_STOP,&mut value)}==0{return Err(native("Cannot stop bridge support before package maintenance"));}
    }
    wait(service,SERVICE_STOPPED)?;
    if unsafe{WaitForSingleObject(process.as_raw_handle(),30_000)}!=WAIT_OBJECT_0{return Err(error("The bridge helper has not exited. Keep its files installed and retry."));}
    Ok(())
}
fn start(service:&Service)->Result<(),BridgeError>{
    if unsafe{StartServiceW(service.0,0,ptr::null())}==0 && unsafe{GetLastError()}!=ERROR_SERVICE_ALREADY_RUNNING{return Err(native("Cannot start bridge support"));}
    Ok(())
}
fn mode(service:&Service,helper:&InstalledHelper,cleanup:bool)->Result<(),BridgeError>{
    let executable=wide(&command(helper,cleanup));
    if unsafe{ChangeServiceConfigW(service.0,SERVICE_NO_CHANGE,SERVICE_NO_CHANGE,SERVICE_NO_CHANGE,executable.as_ptr(),ptr::null(),ptr::null_mut(),ptr::null(),ptr::null(),ptr::null(),ptr::null())}==0{return Err(native("Cannot select bridge package maintenance"));}
    Ok(())
}
fn start_type(service:&Service,value:u32)->Result<(),BridgeError>{
    if unsafe{ChangeServiceConfigW(service.0,SERVICE_NO_CHANGE,value,SERVICE_NO_CHANGE,ptr::null(),ptr::null(),ptr::null_mut(),ptr::null(),ptr::null(),ptr::null(),ptr::null())}==0{return Err(native("Cannot set bridge maintenance startup mode"));}
    Ok(())
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation{fn drop(&mut self){unsafe{LocalFree(self.0);}}}
struct PackageLock(OwnedHandle);
impl Drop for PackageLock{fn drop(&mut self){unsafe{ReleaseMutex(self.0.as_raw_handle());}}}
impl PackageLock{
    fn open()->Result<Self,BridgeError>{
        let mut descriptor=ptr::null_mut();
        let sddl=wide("O:BAG:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)");
        if unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),1,&mut descriptor,ptr::null_mut())}==0{return Err(native("Cannot protect package maintenance"));}
        let _allocation=Allocation(descriptor);
        let attributes=SECURITY_ATTRIBUTES{nLength:std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,lpSecurityDescriptor:descriptor,bInheritHandle:0};
        let name=wide("Global\\net.serialx.eink-qemu.bridge.package");
        let handle=unsafe{CreateMutexExW(&attributes,name.as_ptr(),0,MUTEX_ALL_ACCESS)};
        if handle.is_null(){return Err(native("Cannot lock bridge package maintenance"));}
        let handle=unsafe{OwnedHandle::from_raw_handle(handle)};
        windows_security::validate_maintenance_mutex(handle.as_raw_handle())?;
        match unsafe{WaitForSingleObject(handle.as_raw_handle(),0)}{
            WAIT_OBJECT_0|WAIT_ABANDONED=>Ok(Self(handle)),
            WAIT_TIMEOUT=>Err(BridgeError::new("EBUSY","Another bridge package operation is running.")),
            _=>Err(native("Cannot acquire bridge package maintenance")),
        }
    }
}
fn directory(path:&Path)->Result<File,BridgeError>{
    let mut descriptor=ptr::null_mut();
    let sddl=wide("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    if unsafe{ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),1,&mut descriptor,ptr::null_mut())}==0{return Err(native("Cannot protect bridge recovery storage"));}
    let _allocation=Allocation(descriptor);
    let attributes=SECURITY_ATTRIBUTES{nLength:std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,lpSecurityDescriptor:descriptor,bInheritHandle:0};
    let name:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe{CreateDirectoryW(name.as_ptr(),&attributes)}==0 && unsafe{GetLastError()}!=ERROR_ALREADY_EXISTS{return Err(native("Cannot create bridge recovery storage"));}
    let file=OpenOptions::new().read(true).share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT|FILE_FLAG_BACKUP_SEMANTICS).open(path).map_err(|_|error("Cannot lock bridge recovery storage"))?;
    if !file.metadata().map_err(|_|error("Cannot inspect bridge recovery storage"))?.is_dir(){return Err(error("Expected a protected recovery directory"));}
    windows_security::validate_private_handle(&file,"S-1-5-18")?;
    Ok(file)
}
fn journal()->Result<Vec<File>,BridgeError>{
    let root=windows_elevation::journal_root()?;
    let parent=root.parent().ok_or_else(||error("Invalid recovery location"))?;
    // Pin the known-folder ancestors against rename/reparse changes before
    // creating private children. ProgramData itself permits unrelated folders.
    let mut locks=Vec::new();
    for path in parent.parent().ok_or_else(||error("Invalid recovery location"))?.ancestors().collect::<Vec<_>>().into_iter().rev(){
        let file=OpenOptions::new().read(true).share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT|FILE_FLAG_BACKUP_SEMANTICS).open(path).map_err(|_|error("Cannot lock Windows data directory"))?;
        let mut info=BY_HANDLE_FILE_INFORMATION::default();
        if unsafe{GetFileInformationByHandle(file.as_raw_handle(),&mut info)}==0 || info.dwFileAttributes&FILE_ATTRIBUTE_REPARSE_POINT!=0 || info.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY==0{return Err(error("Windows data directories cannot be reparse points"));}
        locks.push(file);
    }
    locks.push(directory(parent)?);locks.push(directory(&root)?);Ok(locks)
}

fn installation()->Result<Vec<File>,BridgeError>{
    let root=windows_elevation::installation_root()?;
    let app=root.parent().ok_or_else(||error("Invalid installation directory"))?;
    let mut locks=Vec::new();
    for path in root.ancestors().collect::<Vec<_>>().into_iter().rev(){
        let file=match OpenOptions::new().read(true).share_mode(FILE_SHARE_READ|FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT|FILE_FLAG_BACKUP_SEMANTICS).open(path){
            Ok(file)=>file,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>continue,
            Err(_)=>return Err(error("Cannot inspect the fixed bridge installation directory")),
        };
        let mut info=BY_HANDLE_FILE_INFORMATION::default();
        if unsafe{GetFileInformationByHandle(file.as_raw_handle(),&mut info)}==0 || info.dwFileAttributes&FILE_ATTRIBUTE_REPARSE_POINT!=0 || info.dwFileAttributes&FILE_ATTRIBUTE_DIRECTORY==0{return Err(error("Bridge installation directories cannot be reparse points"));}
        if path.starts_with(app){windows_security::validate_installed_handle(&file)?;}
        locks.push(file);
    }
    Ok(locks)
}
fn restart_after_crash(service:&Service)->Result<(),BridgeError>{
    let mut actions=[SC_ACTION{Type:SC_ACTION_RESTART,Delay:5_000},SC_ACTION{Type:SC_ACTION_RESTART,Delay:30_000},SC_ACTION{Type:SC_ACTION_RESTART,Delay:60_000}];
    let value=SERVICE_FAILURE_ACTIONSW{dwResetPeriod:86_400,lpRebootMsg:ptr::null_mut(),lpCommand:ptr::null_mut(),cActions:actions.len() as u32,lpsaActions:actions.as_mut_ptr()};
    if unsafe{ChangeServiceConfig2W(service.0,SERVICE_CONFIG_FAILURE_ACTIONS,(&value as *const SERVICE_FAILURE_ACTIONSW).cast())}==0{return Err(native("Cannot enable bridge service crash recovery"));}
    // A deliberate cleanup refusal must not restart in cleanup mode. Crash
    // recovery still restarts the fixed service image, without a shell command.
    let flag=SERVICE_FAILURE_ACTIONS_FLAG{fFailureActionsOnNonCrashFailures:0};
    if unsafe{ChangeServiceConfig2W(service.0,SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,(&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast())}==0{return Err(native("Cannot configure bridge cleanup failure handling"));}
    Ok(())
}

/// Fixed verbs for the bundled installer. No destination, executable, driver,
/// owner, network command, or service name is accepted from the command line.
pub fn maintain(action:&str)->Result<(),BridgeError>{
    admin()?;
    if !matches!(action,"install"|"prepare-update"|"remove"){return Err(error("Unknown bridge package action"));}
    let _maintenance=PackageLock::open()?;
    let _installation=installation()?;
    let manager=manager(action=="install")?;
    let existing=open(&manager)?;
    if existing.is_none() && action!="install"{return Ok(());}
    let helper=InstalledHelper::open()?;
    let previous_start=existing.as_ref().map(|service|validate(service,&helper)).transpose()?;
    if action=="prepare-update"{return stop(existing.as_ref().unwrap());}
    let _package=VerifiedPackage::open(&windows_elevation::installation_root()?.join("driver"))?;
    if action=="install"{
        let _directories=journal()?;
        let service=match existing{
            Some(service)=>{stop(&service)?;mode(&service,&helper,false)?;service},
            None=>{
                let name=wide(windows_scm::SERVICE_NAME);let display=wide("eink-qemu bridge support");let executable=wide(&command(&helper,false));
                let handle=unsafe{CreateServiceW(manager.0,name.as_ptr(),display.as_ptr(),SERVICE_ALL_ACCESS,SERVICE_WIN32_OWN_PROCESS,SERVICE_AUTO_START,SERVICE_ERROR_NORMAL,
                    executable.as_ptr(),ptr::null(),ptr::null_mut(),ptr::null(),ptr::null(),ptr::null())};
                if handle.is_null(){return Err(native("Cannot register bridge support"));}
                Service(handle)
            },
        };
        let start_type=validate(&service,&helper)?;
        restart_after_crash(&service)?;
        if start_type==SERVICE_DISABLED{return Ok(());}
        start(&service)?;wait(&service,SERVICE_RUNNING)?;return Ok(());
    }
    let service=existing.unwrap();stop(&service)?;
    mode(&service,&helper,true)?;
    if previous_start==Some(SERVICE_DISABLED){start_type(&service,SERVICE_DEMAND_START)?;}
    let result=start(&service).and_then(|_|wait(&service,SERVICE_STOPPED)).and_then(|status|{
        if status.dwWin32ExitCode==0 && status.dwServiceSpecificExitCode==0{Ok(())}
        else{Err(error("Bridge cleanup could not finish. Close emulators and review bridge setup before removing this package."))}
    });
    // On cleanup refusal, restore the ordinary service so its owner can repair
    // the recorded conflict. Never delete the helper or journal on failure.
    mode(&service,&helper,false)?;
    if previous_start==Some(SERVICE_DISABLED){start_type(&service,SERVICE_DISABLED)?;}
    if let Err(error)=result{if previous_start!=Some(SERVICE_DISABLED){let _=start(&service);}return Err(error);}
    if unsafe{DeleteService(service.0)}==0{return Err(native("Cannot unregister bridge support"));}
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    use windows_sys::Win32::System::JobObjects::*;
    use crate::{model::*,protocol::*,transport::HelperClient};
    #[test]
    #[ignore="requires LocalSystem and the installed service in the disposable Windows VM; creates and removes a temporary standard account"]
    fn native_standard_account_cannot_maintain_or_approve_setup(){
        const CHILD:&str="EINK_MAINTENANCE_STANDARD_CHILD";
        if std::env::var_os(CHILD).is_some(){
            let diagnostic=format!(r"C:\Windows\Temp\eink-maintenance-standard-{}.log",std::process::id());
            std::panic::set_hook(Box::new(move|info|{let _=std::fs::write(&diagnostic,info.to_string());}));
            assert_ne!(windows_security::current_sid().unwrap(),"S-1-5-18");
            assert_eq!(admin().unwrap_err().code,"EPERM","The test must run without administrator privileges");
            for action in ["install","prepare-update","remove"]{assert_eq!(maintain(action).unwrap_err().code,"EPERM");}
            assert_eq!(crate::windows_runtime::uninstall().unwrap_err().code,"EPERM");
            let request=|request|HelperClient::connect().unwrap().request(HelperRequest{version:VERSION,request});
            let ReplyKind::Inspection{inspection}=request(RequestKind::Inspect).unwrap().reply else{panic!("Missing inspection")};
            assert_eq!(inspection.support,SupportState::Ready);
            assert!(inspection.capabilities.create);
            assert!(inspection.selection.is_none());
            assert_eq!(request(RequestKind::Apply{plan_id:OperationId::new("c".repeat(32)).unwrap(),authorization:None}).unwrap_err().code,"EAUTHDENIED");
            return;
        }
        windows_security::require_system().unwrap();
        assert_eq!(std::env::var("COMPUTERNAME").as_deref(),Ok("WIN-SUNSKHPNVMO"));
        assert_eq!(std::env::var("EINK_BRIDGE_DISPOSABLE_WINDOWS_TESTS").as_deref(),Ok("1"));
        let maintenance=PackageLock::open().unwrap();
        assert_eq!(std::thread::spawn(||PackageLock::open().err().unwrap().code).join().unwrap(),"EBUSY");
        drop(maintenance);
        use windows_sys::Win32::NetworkManagement::NetManagement::*;
        let mut username=wide(&format!("einkQA{:08x}",getrandom::u32().unwrap()));
        let mut password=wide(&format!("aA1!{:016x}{:016x}",getrandom::u64().unwrap(),getrandom::u64().unwrap()));
        let info=USER_INFO_1{usri1_name:username.as_mut_ptr(),usri1_password:password.as_mut_ptr(),usri1_priv:USER_PRIV_USER,
            usri1_flags:UF_SCRIPT|UF_NORMAL_ACCOUNT,..Default::default()};
        assert_eq!(unsafe{NetUserAdd(ptr::null(),1,(&info as *const USER_INFO_1).cast(),ptr::null_mut())},0);
        struct Account(Option<Vec<u16>>);
        impl Drop for Account{fn drop(&mut self){if let Some(name)=&self.0{unsafe{NetUserDel(ptr::null(),name.as_ptr());}}}}
        let mut account=Account(Some(username));
        let mut token=ptr::null_mut();
        let result=unsafe{LogonUserW(account.0.as_ref().unwrap().as_ptr(),wide(".").as_ptr(),password.as_ptr(),LOGON32_LOGON_INTERACTIVE,LOGON32_PROVIDER_DEFAULT,&mut token)};
        for value in &mut password{unsafe{ptr::write_volatile(value,0);}}
        assert_ne!(result,0,"{}",std::io::Error::last_os_error());
        let token=unsafe{OwnedHandle::from_raw_handle(token)};
        assert!(!windows_security::token_is_elevated_admin(token.as_raw_handle()).unwrap());
        let executable=wide(std::env::current_exe().unwrap().to_str().unwrap());
        let mut command=wide(&format!("\"{}\" --exact windows_install::tests::native_standard_account_cannot_maintain_or_approve_setup --include-ignored",std::env::current_exe().unwrap().display()));
        let environment=wide(&format!("{CHILD}=1\0SystemRoot=C:\\Windows\0TEMP=C:\\Windows\\Temp\0"));
        let mut startup=STARTUPINFOW::default();startup.cb=std::mem::size_of_val(&startup) as u32;
        let mut process=PROCESS_INFORMATION::default();
        let handle=unsafe{CreateJobObjectW(ptr::null(),ptr::null())};assert!(!handle.is_null());
        let job=unsafe{OwnedHandle::from_raw_handle(handle)};
        let mut limits=JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        assert_ne!(unsafe{SetInformationJobObject(job.as_raw_handle(),JobObjectExtendedLimitInformation,(&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),std::mem::size_of_val(&limits) as u32)},0);
        assert_ne!(unsafe{CreateProcessAsUserW(token.as_raw_handle(),executable.as_ptr(),command.as_mut_ptr(),ptr::null(),ptr::null(),0,
            CREATE_UNICODE_ENVIRONMENT|CREATE_NO_WINDOW|CREATE_SUSPENDED,environment.as_ptr().cast(),wide(r"C:\Windows\Temp").as_ptr(),&startup,&mut process)},0,"{}",std::io::Error::last_os_error());
        struct Child(OwnedHandle);
        impl Drop for Child{fn drop(&mut self){if unsafe{WaitForSingleObject(self.0.as_raw_handle(),0)}==WAIT_TIMEOUT{unsafe{TerminateProcess(self.0.as_raw_handle(),1);WaitForSingleObject(self.0.as_raw_handle(),5000);}}}}
        let child=Child(unsafe{OwnedHandle::from_raw_handle(process.hProcess)});
        let thread=unsafe{OwnedHandle::from_raw_handle(process.hThread)};
        assert_ne!(unsafe{AssignProcessToJobObject(job.as_raw_handle(),child.0.as_raw_handle())},0);
        assert_eq!(unsafe{ResumeThread(thread.as_raw_handle())},1);
        assert_eq!(unsafe{WaitForSingleObject(child.0.as_raw_handle(),180_000)},WAIT_OBJECT_0);
        let mut code=1;assert_ne!(unsafe{GetExitCodeProcess(child.0.as_raw_handle(),&mut code)},0);
        assert_eq!(code,0,"{}",std::fs::read_to_string(format!(r"C:\Windows\Temp\eink-maintenance-standard-{}.log",process.dwProcessId)).unwrap_or_default());
        assert_eq!(unsafe{NetUserDel(ptr::null(),account.0.as_ref().unwrap().as_ptr())},0);account.0=None;
    }
}
