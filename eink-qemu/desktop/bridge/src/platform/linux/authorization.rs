//! The privileged service asks polkit about the kernel-authenticated client.
//! Authentication UI belongs to the user's existing desktop polkit agent.
use crate::{model::BridgeError, transport::Caller};
use std::{collections::HashMap, time::Duration};
use zbus::{blocking::{Proxy, connection::Builder}, zvariant::OwnedValue};
pub const ACTION: &str = "net.serialx.eink-qemu.bridge.configure";
fn unavailable(error: impl std::fmt::Display) -> BridgeError {
    BridgeError { code: "EAUTHUNAVAILABLE".into(), message: "System authorization is unavailable. Check that polkit and a desktop authentication agent are running.".into(), detail: Some(error.to_string()) }
}
fn decision(authorized: bool, challenge: bool, details: &HashMap<String, String>) -> Result<(), BridgeError> {
    if authorized { return Ok(()); }
    if details.get("polkit.dismissed").is_some_and(|value| value == "true") {
        return Err(BridgeError::new("EAUTHCANCELLED", "Administrator authorization was cancelled."));
    }
    Err(BridgeError::new("EAUTHDENIED", if challenge { "Administrator authorization was not completed. Check the system authentication prompt." }
        else { "System policy does not authorize bridge setup for this session." }))
}
fn native_error(error: zbus::Error) -> BridgeError {
    if let zbus::Error::MethodError(name, _, _) = &error {
        if name.as_str() == "org.freedesktop.PolicyKit1.Error.Cancelled" { return BridgeError::new("EAUTHCANCELLED", "Administrator authorization was cancelled."); }
        if name.as_str() == "org.freedesktop.PolicyKit1.Error.NotAuthorized" { return BridgeError::new("EAUTHDENIED", "System policy denied bridge setup authorization."); }
    }
    unavailable(error)
}
pub fn authorize(caller: &Caller) -> Result<(), BridgeError> {
    if unsafe { libc::geteuid() } != 0 { return Err(BridgeError::new("EPERM", "Only the installed bridge service can authorize network changes.")); }
    let process = caller.process();
    process.revalidate()?;
    let uid = i32::try_from(process.uid()).map_err(|_| BridgeError::new("EPERM", "This account cannot be represented by system authorization."))?;
    let subject = ("unix-process", HashMap::<&str, OwnedValue>::from([
        ("pid", process.pid().into()), ("start-time", process.start_time().into()), ("uid", uid.into()),
    ]));
    let connection = Builder::system().map_err(unavailable)?.method_timeout(Duration::from_secs(180)).build().map_err(unavailable)?;
    let proxy = Proxy::new(&connection, "org.freedesktop.PolicyKit1", "/org/freedesktop/PolicyKit1/Authority", "org.freedesktop.PolicyKit1.Authority").map_err(unavailable)?;
    let details = HashMap::<String, String>::new();
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(unavailable)?;
    let cancellation = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    // No client-supplied action names or cancellation identities. A timed-out
    // call is explicitly cancelled before this dedicated connection closes.
    let result: Result<(bool, bool, HashMap<String, String>), zbus::Error> = proxy.call("CheckAuthorization", &(subject, ACTION, details, 1u32, &cancellation));
    let (authorized, challenge, details) = match result {
        Ok(result) => result,
        Err(error) => {
            let _: Result<(), _> = proxy.call("CancelCheckAuthorization", &(&cancellation,));
            return Err(native_error(error));
        },
    };
    process.revalidate()?;
    decision(authorized, challenge, &details)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_denial_or_dismissal_never_becomes_authorized() {
        assert!(decision(true, false, &HashMap::new()).is_ok());
        for challenge in [true, false] {
            assert_eq!(decision(false, challenge, &HashMap::new()).unwrap_err().code, "EAUTHDENIED");
            assert_eq!(decision(false, challenge, &HashMap::from([("polkit.dismissed".into(), "true".into())])).unwrap_err().code, "EAUTHCANCELLED");
        }
    }
}
