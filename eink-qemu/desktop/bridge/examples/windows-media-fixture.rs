//! Bounded media-up fixture for a disposable, dedicated TAP. Never shipped in IPC.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{model::ResourceId,windows_device::{self,DeviceIdentity},windows_driver,windows_security,windows_topology::Topology,windows_verify};
    use serde::Deserialize;
    use std::{fs,io::Write,os::windows::io::{AsRawHandle,FromRawHandle,OwnedHandle},path::Path,ptr,time::{Duration,Instant}};
    use windows_sys::Win32::{Foundation::*,Storage::FileSystem::*,System::{IO::*,Threading::*}};
    #[derive(Deserialize)]struct Fixture{resource:ResourceId,device:Option<DeviceIdentity>}
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable host".into());}
    let root=Path::new(r"C:\eink-driver-native-gate");
    let file=fs::File::open(root.join("network-device.json"))?;windows_security::validate_handle(&file,None)?;
    let fixture:Fixture=serde_json::from_reader(file)?;let target=fixture.device.ok_or("Missing owned TAP")?;
    if target.instance_id!=windows_driver::instance_id(&fixture.resource) || windows_device::enumerate()?.get(&target.guid)!=Some(&target){return Err("Fixture identity changed".into());}
    let ready=root.join(format!("media-ready-{}",fixture.resource.as_str()));
    let stop=root.join(format!("media-stop-{}",fixture.resource.as_str()));
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args==["--stop"]{
        let mut marker=fs::OpenOptions::new().write(true).create(true).truncate(true).open(stop)?;
        windows_security::validate_handle(&marker,None)?;marker.write_all(b"stop")?;marker.sync_all()?;
        let deadline=Instant::now()+Duration::from_secs(30);
        loop{match windows_verify::probe(&target.guid){Ok(_)=>break,Err(error) if error.code=="EBUSY" && Instant::now()<deadline=>std::thread::sleep(Duration::from_millis(200)),Err(error)=>return Err(error.into())}}
        println!("nativeMediaReleased=passed");return Ok(());
    }
    if args!=["--hold"]{return Err("Expected --hold or --stop".into());}
    if stop.exists() || ready.exists(){return Err("Media fixture already ran".into());}
    Topology::read()?.require_unbridged(&target)?;windows_verify::probe(&target.guid)?;
    let path:Vec<u16>=format!(r"\\.\Global\{}.tap",target.guid).encode_utf16().chain(Some(0)).collect();
    let handle=unsafe{CreateFileW(path.as_ptr(),GENERIC_READ|GENERIC_WRITE,0,ptr::null(),OPEN_EXISTING,FILE_ATTRIBUTE_SYSTEM|FILE_FLAG_OVERLAPPED,ptr::null_mut())};
    if handle==INVALID_HANDLE_VALUE{return Err(std::io::Error::last_os_error().into());}
    let file=unsafe{OwnedHandle::from_raw_handle(handle)};
    let event=unsafe{CreateEventW(ptr::null(),1,0,ptr::null())};
    if event.is_null(){return Err(std::io::Error::last_os_error().into());}
    let event=unsafe{OwnedHandle::from_raw_handle(event)};
    let mut operation:OVERLAPPED=unsafe{std::mem::zeroed()};operation.hEvent=event.as_raw_handle();
    let mut media=1u32;let mut bytes=0;
    // Same TAP_CONTROL_CODE(6, METHOD_BUFFERED) and buffer as QEMU.
    let result=unsafe{DeviceIoControl(file.as_raw_handle(),0x00220018,(&media as *const u32).cast(),4,(&mut media as *mut u32).cast(),4,&mut bytes,&mut operation)};
    if result==0{
        if unsafe{GetLastError()}!=ERROR_IO_PENDING{return Err(std::io::Error::last_os_error().into());}
        if unsafe{WaitForSingleObject(event.as_raw_handle(),3000)}!=WAIT_OBJECT_0{
            unsafe{CancelIoEx(file.as_raw_handle(),&operation);GetOverlappedResult(file.as_raw_handle(),&operation,&mut bytes,1);}
            return Err("Media request timed out".into());
        }
        if unsafe{GetOverlappedResult(file.as_raw_handle(),&operation,&mut bytes,0)}==0{return Err(std::io::Error::last_os_error().into());}
    }
    let mut marker=fs::OpenOptions::new().write(true).create_new(true).open(ready)?;
    windows_security::validate_handle(&marker,None)?;marker.write_all(std::process::id().to_string().as_bytes())?;marker.sync_all()?;drop(marker);
    let deadline=Instant::now()+Duration::from_secs(900);
    while !stop.exists() && Instant::now()<deadline{std::thread::sleep(Duration::from_millis(200));}
    // Closing the sole TAP handle releases media even if the test process dies.
    drop(file);println!("nativeMediaClosed=passed");Ok(())
}
