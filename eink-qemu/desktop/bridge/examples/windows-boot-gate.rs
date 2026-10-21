//! Disposable native restart gate; never changes network configuration.
#[cfg(not(windows))]
fn main(){panic!("Requires the disposable Windows VM");}
#[cfg(windows)]
fn main(){if let Err(error)=run(){eprintln!("{error:?}");std::process::exit(1);}}
#[cfg(windows)]
fn run()->Result<(),Box<dyn std::error::Error>>{
    use eink_bridge::{windows_boot,windows_security};
    use std::{fs,io::Write,path::Path};
    windows_security::require_system()?;
    if std::env::var("COMPUTERNAME").as_deref()!=Ok("WIN-SUNSKHPNVMO"){return Err("Wrong disposable VM".into());}
    let path=Path::new(r"C:\eink-driver-native-gate\boot-marker.json");
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    let current=windows_boot::current()?;
    if args==["--record"]{
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        windows_security::validate_handle(&file,None)?;
        serde_json::to_writer(&mut file,&current)?;file.flush()?;file.sync_all()?;
        println!("nativeBootIdentityRecorded=passed");
    }else if args==["--same"] || args==["--different"]{
        let file=fs::File::open(path)?;windows_security::validate_handle(&file,None)?;
        let before:windows_boot::BootIdentity=serde_json::from_reader(file)?;
        if (before==current)!=(args==["--same"]){return Err("Unexpected Windows kernel session identity".into());}
        println!("nativeBootIdentity{}=passed",if args==["--same"]{"Stable"}else{"Changed"});
    }else{return Err("Expected --record, --same, or --different".into());}
    Ok(())
}
