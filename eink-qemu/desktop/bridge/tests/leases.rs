#![cfg(unix)]
use eink_bridge::transport::unix::{receive_lease_file, send_lease_file};
use std::{fs::OpenOptions, io::Write, os::unix::net::UnixStream};

#[test]
fn transferred_lease_survives_the_sending_helper_and_releases_with_the_client() {
    let path = tempfile::NamedTempFile::new().unwrap();
    let sender = OpenOptions::new().read(true).open(path.path()).unwrap();
    sender.try_lock_shared().unwrap();
    let contender = OpenOptions::new().read(true).open(path.path()).unwrap();
    let (server, client) = UnixStream::pair().unwrap();
    send_lease_file(&server, &sender).unwrap();
    let lease = receive_lease_file(&client).unwrap();
    drop(sender); drop(server);
    assert!(contender.try_lock().is_err(), "helper exit must not release the emulator's lease");
    drop(lease);
    contender.try_lock().unwrap();
}

#[test]
fn lease_transport_requires_a_file_descriptor_and_refuses_directories() {
    let (mut server, client) = UnixStream::pair().unwrap();
    server.write_all(&[1]).unwrap();
    assert_eq!(receive_lease_file(&client).unwrap_err().code, "EPROTOCOL");
    let directory = tempfile::tempdir().unwrap();
    let (server, client) = UnixStream::pair().unwrap();
    send_lease_file(&server, &std::fs::File::open(directory.path()).unwrap()).unwrap();
    assert_eq!(receive_lease_file(&client).unwrap_err().code, "EPROTOCOL");
}
