//! One-use approval tickets held only by the service. Restarting the helper
//! discards approvals; a journal never stores credentials. A separate elevated
//! client approves a ticket, but only its original process can redeem it.
use super::AuthorizationProof;
use crate::{model::{BridgeError, BridgePlan}, transport::windows::Peer};
use std::time::{Duration, Instant};

const MAX_TICKETS: usize = 32;
fn denied() -> BridgeError { BridgeError::new("EAUTHDENIED", "Approve the current bridge plan through the Windows administrator prompt.") }
#[derive(Clone, PartialEq, Eq)]
struct Owner { principal: String, pid: u32, creation_time: u64 }
impl Owner {
    fn peer(peer: &Peer) -> Result<Self, BridgeError> {
        peer.revalidate()?;
        Ok(Self { principal: peer.caller().principal().into(), pid: peer.pid, creation_time: peer.creation_time })
    }
}
struct Ticket { proof: AuthorizationProof, owner: Owner, plan: BridgePlan, revision: u64, expires: Instant, approved: bool }
#[derive(Default)]
pub struct GrantStore { tickets: Vec<Ticket> }
fn same(a: &AuthorizationProof, b: &AuthorizationProof) -> bool {
    a.0.iter().zip(b.0.iter()).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}
impl GrantStore {
    /// `plan` and `revision` must come from the owner's protected coordinator
    /// record, never from request JSON. The dispatcher serializes this store.
    pub fn begin(&mut self, peer: &Peer, plan: BridgePlan, revision: u64) -> Result<AuthorizationProof, BridgeError> {
        self.begin_for(Owner::peer(peer)?, plan, revision, Instant::now())
    }
    pub fn approve(&mut self, peer: &Peer, proof: &AuthorizationProof) -> Result<(), BridgeError> {
        peer.require_elevated_admin()?;
        self.approve_checked(proof, Instant::now())
    }
    /// Consume before Apply. Even a later native failure needs fresh approval.
    /// Another user/process cannot redeem or invalidate the owner's ticket.
    pub fn consume(&mut self, peer: &Peer, proof: &AuthorizationProof, plan: &BridgePlan, revision: u64) -> Result<(), BridgeError> {
        self.consume_for(&Owner::peer(peer)?, proof, plan, revision, Instant::now())
    }
    fn prune(&mut self, now: Instant) { self.tickets.retain(|ticket| now < ticket.expires); }
    fn begin_for(&mut self, owner: Owner, plan: BridgePlan, revision: u64, now: Instant) -> Result<AuthorizationProof, BridgeError> {
        self.prune(now);
        self.tickets.retain(|ticket| ticket.owner != owner || ticket.plan.id != plan.id);
        if self.tickets.len() >= MAX_TICKETS { return Err(BridgeError::new("EBUSY", "Too many bridge approval requests. Try again shortly.")); }
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| denied())?;
        let proof = AuthorizationProof(bytes);
        self.tickets.push(Ticket { proof: proof.clone(), owner, plan, revision,
            expires: now + Duration::from_secs(300), approved: false });
        Ok(proof)
    }
    fn approve_checked(&mut self, proof: &AuthorizationProof, now: Instant) -> Result<(), BridgeError> {
        self.prune(now);
        let ticket = self.tickets.iter_mut().find(|ticket| same(&ticket.proof, proof)).ok_or_else(denied)?;
        if ticket.approved { return Err(denied()); }
        ticket.approved = true;
        ticket.expires = now + Duration::from_secs(60);
        Ok(())
    }
    fn consume_for(&mut self, owner: &Owner, proof: &AuthorizationProof, plan: &BridgePlan, revision: u64, now: Instant) -> Result<(), BridgeError> {
        self.prune(now);
        let index = self.tickets.iter().position(|ticket| &ticket.owner == owner && same(&ticket.proof, proof)).ok_or_else(denied)?;
        let ticket = self.tickets.swap_remove(index);
        if !ticket.approved || &ticket.plan != plan || ticket.revision != revision { return Err(denied()); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AdapterId, OperationId, PlanRequest};
    fn owner() -> Owner { Owner { principal: "sid:S-1-5-21-1000".into(), pid: 100, creation_time: 200 } }
    fn plan() -> BridgePlan {
        BridgePlan { id: OperationId::new("12".repeat(16)).unwrap(), request: PlanRequest::Configure { adapter_id: AdapterId::new("ethernet").unwrap() },
            fingerprint: "reviewed-host".into(), changes: vec!["Create bridge".into()], requires_authorization: true, may_disconnect: true, expires_at: 1000 }
    }
    #[test]
    fn approval_is_one_use_and_bound_to_owner_process_and_review() {
        let now = Instant::now(); let plan = plan(); let owner = owner(); let mut store = GrantStore::default();
        let proof = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap();
        store.approve_checked(&proof, now).unwrap();
        assert!(store.approve_checked(&proof, now).is_err());
        for stranger in [Owner { principal: "sid:S-1-5-21-1001".into(), ..owner.clone() },
            Owner { pid: 101, ..owner.clone() }, Owner { creation_time: 201, ..owner.clone() }] {
            assert!(store.consume_for(&stranger, &proof, &plan, 1, now).is_err());
        }
        store.consume_for(&owner, &proof, &plan, 1, now).unwrap();
        assert!(store.consume_for(&owner, &proof, &plan, 1, now).is_err());
        for changed in [BridgePlan { fingerprint: "changed".into(), ..plan.clone() }, BridgePlan { changes: vec!["Changed operation".into()], ..plan.clone() }] {
            let proof = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap(); store.approve_checked(&proof, now).unwrap();
            assert!(store.consume_for(&owner, &proof, &changed, 1, now).is_err());
            assert!(store.consume_for(&owner, &proof, &plan, 1, now).is_err());
        }
        let proof = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap(); store.approve_checked(&proof, now).unwrap();
        assert!(store.consume_for(&owner, &proof, &plan, 2, now).is_err());
    }
    #[test]
    fn unapproved_expired_restarted_and_replaced_tickets_are_denied() {
        let now = Instant::now(); let plan = plan(); let owner = owner(); let mut store = GrantStore::default();
        let proof = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap();
        assert!(store.consume_for(&owner, &proof, &plan, 1, now).is_err());
        assert!(store.approve_checked(&proof, now).is_err());
        let old = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap();
        let fresh = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap();
        assert!(store.approve_checked(&old, now).is_err());
        assert!(store.approve_checked(&fresh, now + Duration::from_secs(300)).is_err());
        let proof = store.begin_for(owner.clone(), plan.clone(), 1, now).unwrap(); store.approve_checked(&proof, now).unwrap();
        assert!(GrantStore::default().consume_for(&owner, &proof, &plan, 1, now).is_err());
        assert!(store.consume_for(&owner, &proof, &plan, 1, now + Duration::from_secs(60)).is_err());
    }
    #[test]
    fn pending_requests_have_a_fixed_memory_bound() {
        let now = Instant::now(); let plan = plan(); let mut store = GrantStore::default();
        for pid in 0..MAX_TICKETS as u32 { store.begin_for(Owner { pid, ..owner() }, plan.clone(), 1, now).unwrap(); }
        assert_eq!(store.begin_for(owner(), plan.clone(), 1, now).unwrap_err().code, "EBUSY");
        assert!(store.begin_for(owner(), plan, 1, now + Duration::from_secs(300)).is_ok());
    }
}
