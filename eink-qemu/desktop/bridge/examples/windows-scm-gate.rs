//! Disposable-VM SCM start/stop/restart/failure gate; no networking mutations.
#[cfg(not(windows))]
fn main(){panic!("Requires Windows");}
#[cfg(windows)]
fn main(){if run().is_err(){std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),eink_bridge::model::BridgeError>{
    use eink_bridge::{model::BridgeError,windows_scm,windows_security};
    use std::{fs,path::Path,sync::atomic::Ordering,time::Duration};
    let failure=|message:&str|BridgeError::new("ETEST",message);
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO") || std::env::args().skip(1).collect::<Vec<_>>()!=["--serve"]{return Err(failure("Wrong fixture invocation"));}
    windows_scm::dispatch(move |stop,ready|{
        let root=Path::new(r"C:\eink-driver-native-gate\scm-native-v1");
        let file=fs::File::open(root.join("run-id")).map_err(|_|failure("Missing run identity"))?;
        windows_security::validate_private_handle(&file,"S-1-5-18")?;
        let id=fs::read_to_string(root.join("run-id")).map_err(|_|failure("Unreadable run identity"))?;
        if !["1","2","3"].contains(&id.as_str()){return Err(failure("Unknown fixture run"));}
        if id=="3"{return Err(failure("Deliberate startup failure"));}
        ready.running()?;
        while !stop.load(Ordering::Acquire){std::thread::sleep(Duration::from_millis(50));}
        // Simulate final recovery work. SCM must not report STOPPED until done.
        std::thread::sleep(Duration::from_secs(2));
        let path=root.join(format!("cleaned-{id}"));
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|_|failure("Cleanup marker already exists"))?;
        windows_security::validate_private_handle(&file,"S-1-5-18")?;
        std::io::Write::write_all(&mut file,b"completed").map_err(|_|failure("Cannot record cleanup"))?;
        file.sync_all().map_err(|_|failure("Cannot persist cleanup"))?;
        Ok(())
    })
}
