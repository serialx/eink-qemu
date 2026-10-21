//! Durable transaction engine. The authenticated helper owns this coordinator
//! and calls recover at startup and on a timer, independently of the WebView.
use crate::{journal::Journal, model::*};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::Path;

pub trait PlatformOps {
    type Snapshot: Serialize + DeserializeOwned + Clone;
    type Mutation: Serialize + DeserializeOwned + Clone;
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&Resource<Self::Snapshot>>) -> Result<Prepared<Self::Snapshot, Self::Mutation>, BridgeError>;
    fn fingerprint(&mut self, request: &PlanRequest, snapshot: &Self::Snapshot) -> Result<String, BridgeError>;
    fn apply(&mut self, mutation: &Self::Mutation) -> Result<(), BridgeError>;
    /// Record native handles allocated during a step (for example an NM
    /// checkpoint). Preserve the original undo state. The coordinator persists
    /// this context before invoking the next external mutation. A step that
    /// allocates a handle must remain recoverable if it crashes before return.
    fn apply_recorded(&mut self, mutation: &Self::Mutation, _: &mut Self::Snapshot) -> Result<(), BridgeError> { self.apply(mutation) }
    fn verify(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError>;
    /// Restore volatile runtime state for an already committed resource at boot.
    /// Preserve changed persistent configuration; this is not automatic repair.
    fn resume(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError> { self.verify(snapshot) }
    /// Disarm native timed rollback only after verification and confirmation.
    /// Must be idempotent. The durable snapshot remains usable if the helper
    /// crashes after this call but before recording the committed resource.
    fn commit(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError>;
    /// Record finalized native state with the committed resource. A crash
    /// before the final journal write still restores the prior undo snapshot,
    /// so backends must tolerate partial native commit during rollback.
    fn commit_recorded(&mut self, snapshot: &mut Self::Snapshot) -> Result<(), BridgeError> { self.commit(snapshot) }
    /// Must be idempotent and conflict-aware. Called even after partial apply.
    fn rollback(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared<S, M> { pub plan: BridgePlan, pub snapshot: S, pub mutations: Vec<M> }
/// Protected installation history, distinct from the latest operation's undo
/// snapshot. Repair must preserve this original state for eventual removal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource<S> {
    pub id: ResourceId,
    pub owner: String,
    pub adapter_id: AdapterId,
    pub fingerprint: String,
    pub original: S,
}
/// Shared OS lock for an authenticated transport session. Unix helpers transfer
/// a read-only duplicate to the desktop so helper restarts cannot release it.
pub struct ResourceLease { _file: std::fs::File }
impl ResourceLease {
    /// Native transports may duplicate this read-only descriptor to the caller.
    pub fn file(&self) -> &std::fs::File { &self._file }
}
#[derive(Clone, Serialize, Deserialize)]
struct Record<S, M> {
    version: u32, owner: String, prepared: Prepared<S, M>, operation: Operation,
    undo_required: bool,
    #[serde(default)]
    completed_mutations: usize,
    #[serde(default)]
    in_flight_mutation: Option<usize>,
    resources: Vec<Resource<S>>,
}
pub struct Coordinator<P: PlatformOps> { journal: Journal, owner: String, platform: P }
impl<P: PlatformOps> Coordinator<P> {
    /// owner must be the authenticated OS principal, never a request field.
    pub fn open(root: &Path, owner: &str, platform: P) -> Result<Self, BridgeError> {
        if owner.is_empty() { return Err(BridgeError::new("EPERM", "Missing bridge caller identity.")); }
        Ok(Self { journal: Journal::open(root)?, owner: owner.into(), platform })
    }
    fn record(&self) -> Result<Option<Record<P::Snapshot, P::Mutation>>, BridgeError> {
        let record: Option<Record<P::Snapshot, P::Mutation>> = self.journal.read()?;
        if record.as_ref().is_some_and(|r| r.version != VERSION) { return Err(BridgeError::new("EPROTOCOL", "Bridge recovery record needs a compatible helper.")); }
        Ok(record)
    }
    fn owned(&self, id: &OperationId) -> Result<Record<P::Snapshot, P::Mutation>, BridgeError> {
        let record = self.record()?.ok_or_else(|| BridgeError::new("ENOENT", "Bridge operation no longer exists."))?;
        if record.owner != self.owner { return Err(BridgeError::new("EPERM", "This bridge operation belongs to another user.")); }
        if record.operation.id != *id { return Err(BridgeError::new("ESTALE", "Bridge operation has changed.")); }
        Ok(record)
    }
    fn save(&self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        record.operation.revision += 1; self.journal.write(record)?; Ok(record.operation.clone())
    }
    pub fn resources(&self) -> Result<Vec<Resource<P::Snapshot>>, BridgeError> {
        let _lock = self.journal.lock()?;
        Ok(self.record()?.map(|r| r.resources.into_iter().filter(|r| r.owner == self.owner).collect()).unwrap_or_default())
    }
    /// Service startup only, after interrupted transactions have been restored.
    /// A surviving emulator owns its runtime state until its lease is released.
    /// Per-resource failures remain available for explicit repair and must not
    /// prevent other users' healthy bridges from starting.
    pub fn resume_resources(&mut self) -> Result<Vec<(ResourceId, BridgeError)>, BridgeError> {
        let _lock = self.journal.lock()?;
        let Some(record) = self.record()? else { return Ok(Vec::new()); };
        if record.undo_required { return Err(BridgeError::new("ERECOVERY", "Restore the interrupted bridge operation first.")); }
        let mut failures = Vec::new();
        for resource in record.resources {
            let _lease = match self.journal.resource_lock(&resource.id, false) {
                Ok(lease) => lease,
                Err(error) if error.code == "EBUSY" => continue,
                Err(error) => return Err(error),
            };
            if let Err(error) = self.platform.resume(&resource.original) { failures.push((resource.id, error)); }
        }
        Ok(failures)
    }
    /// Polling never performs recovery or reveals another user's operation.
    /// Only the service-owned watchdog invokes recovery across principals.
    pub fn operation(&self) -> Result<Option<Operation>, BridgeError> {
        let _lock = self.journal.lock()?;
        Ok(self.record()?.filter(|record| record.owner == self.owner).map(|record| record.operation))
    }
    /// Verify while holding both the transaction lock and the transport lease.
    /// The returned lease prevents repair/removal until the native client exits.
    pub fn verify_resource(&mut self, id: &ResourceId) -> Result<(Resource<P::Snapshot>, ResourceLease), BridgeError> {
        let _lock = self.journal.lock()?;
        let record = self.record()?.ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
        let resource = self.resource(&record.resources, id)?.clone();
        if record.undo_required { return Err(BridgeError::new("EBUSY", "Finish bridge setup before starting an emulator.")); }
        let lease = ResourceLease { _file: self.journal.resource_lock(id, true)? };
        self.platform.verify(&resource.original)?;
        Ok((resource, lease))
    }
    fn resource<'a>(&self, resources: &'a [Resource<P::Snapshot>], id: &ResourceId) -> Result<&'a Resource<P::Snapshot>, BridgeError> {
        let resource = resources.iter().find(|r| r.id == *id).ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
        if resource.owner != self.owner { return Err(BridgeError::new("EPERM", "This bridge setup belongs to another user.")); }
        Ok(resource)
    }
    pub fn lease(&self, id: &ResourceId) -> Result<ResourceLease, BridgeError> {
        let _lock = self.journal.lock()?;
        let record = self.record()?.ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
        self.resource(&record.resources, id)?;
        if record.undo_required { return Err(BridgeError::new("EBUSY", "Finish bridge setup before starting an emulator.")); }
        Ok(ResourceLease { _file: self.journal.resource_lock(id, true)? })
    }
    pub fn plan(&mut self, request: PlanRequest, now: u64) -> Result<BridgePlan, BridgeError> {
        let _lock = self.journal.lock()?;
        let mut resources = Vec::new();
        if let Some(record) = self.record()? {
            if record.undo_required {
                return Err(BridgeError::new(if record.operation.state == OperationState::Failed { "ERECOVERY" } else { "EBUSY" }, "Finish or restore the existing bridge operation first."));
            }
            resources = record.resources;
        }
        let existing = match &request {
            PlanRequest::Configure { adapter_id } => {
                if resources.iter().any(|r| r.adapter_id == *adapter_id) {
                    return Err(BridgeError::new("ECONFLICT", "This adapter already has managed setup. Repair or remove it first."));
                }
                None
            },
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } => Some(self.resource(&resources, resource_id)?),
        };
        let _lease = existing.map(|r| self.journal.resource_lock(&r.id, false)).transpose()?;
        let mut bytes = [0u8; 16]; getrandom::fill(&mut bytes).map_err(|_| BridgeError::new("EIO", "Cannot create a bridge operation identity."))?;
        let id = OperationId::new(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())?;
        let prepared = self.platform.prepare(request, id.clone(), now, existing)?;
        if prepared.plan.id != id { return Err(BridgeError::new("EPROTOCOL", "Invalid platform bridge plan.")); }
        let plan = prepared.plan.clone();
        let record = Record { version: VERSION, owner: self.owner.clone(), prepared, undo_required: false, resources,
            completed_mutations: 0, in_flight_mutation: None,
            operation: Operation { id, revision: 1, state: OperationState::AwaitingAuthorization,
                stage: "Review bridge setup".into(), confirmation_deadline: None, reboot_required: false, error: None } };
        self.journal.write(&record)?; Ok(plan)
    }
    pub fn apply(&mut self, id: &OperationId, now: u64) -> Result<Operation, BridgeError> {
        let started = std::time::Instant::now();
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        let _lease = match &record.prepared.plan.request {
            PlanRequest::Configure { .. } => None,
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } => {
                self.resource(&record.resources, resource_id)?;
                Some(self.journal.resource_lock(resource_id, false)?)
            },
        };
        if record.operation.state != OperationState::AwaitingAuthorization { return Err(BridgeError::new("EBUSY", "Bridge operation has already started.")); }
        if now >= record.prepared.plan.expires_at || self.platform.fingerprint(&record.prepared.plan.request, &record.prepared.snapshot)? != record.prepared.plan.fingerprint {
            return Err(BridgeError::new("ESTALE", "The selected connection changed. Review setup again."));
        }
        record.undo_required = true; record.operation.state = OperationState::Applying;
        record.operation.stage = "Configure bridge".into();
        // Persist restoration data before the first external side effect.
        self.save(&mut record)?;
        for index in 0..record.prepared.mutations.len() {
            record.in_flight_mutation = Some(index);
            self.save(&mut record)?;
            if let Err(error) = self.platform.apply_recorded(&record.prepared.mutations[index], &mut record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
            // A crash before this durable completion leaves the step marked
            // uncertain. Recovery restores the snapshot rather than replaying
            // an external action that may already have succeeded.
            record.completed_mutations = index + 1;
            record.in_flight_mutation = None;
            self.save(&mut record)?;
        }
        record.operation.state = OperationState::Verifying; record.operation.stage = "Check connection".into();
        self.save(&mut record)?;
        if let Err(error) = self.platform.verify(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
        if record.prepared.plan.may_disconnect { record.operation.confirmation_deadline = Some(now.saturating_add(started.elapsed().as_secs()).saturating_add(120)); }
        else { return self.finish(&mut record); }
        self.save(&mut record)
    }
    pub fn keep(&mut self, id: &OperationId, now: u64) -> Result<Operation, BridgeError> {
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        if record.operation.state != OperationState::Verifying || record.operation.reboot_required || record.operation.confirmation_deadline.is_none() {
            return Err(BridgeError::new("ESTALE", "This bridge operation is not waiting for confirmation."));
        }
        if now >= record.operation.confirmation_deadline.unwrap() { return self.rollback(&mut record, None); }
        if let Err(error) = self.platform.verify(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
        self.finish(&mut record)
    }
    fn finish(&mut self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        record.operation.confirmation_deadline = None;
        record.operation.stage = "Finalize bridge setup".into();
        self.save(record)?;
        if let Err(error) = self.platform.commit_recorded(&mut record.prepared.snapshot) {
            return self.rollback(record, Some(error));
        }
        self.commit_resource(record)?;
        record.operation.stage = "Bridge ready".into();
        self.save(record)
    }
    fn commit_resource(&self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<(), BridgeError> {
        match &record.prepared.plan.request {
            PlanRequest::Configure { adapter_id } => record.resources.push(Resource {
                id: ResourceId::new(record.operation.id.as_str())?,
                owner: record.owner.clone(), adapter_id: adapter_id.clone(),
                fingerprint: record.prepared.plan.fingerprint.clone(),
                original: record.prepared.snapshot.clone(),
            }),
            PlanRequest::Repair { resource_id } => {
                let resource = record.resources.iter_mut().find(|r| r.id == *resource_id)
                    .ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
                resource.fingerprint = record.prepared.plan.fingerprint.clone();
            },
            PlanRequest::Remove { resource_id } => record.resources.retain(|r| r.id != *resource_id),
        }
        record.undo_required = false; record.operation.state = OperationState::Ready;
        Ok(())
    }
    pub fn cancel(&mut self, id: &OperationId) -> Result<Operation, BridgeError> {
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        if !record.undo_required {
            if record.operation.state == OperationState::Ready { return Err(BridgeError::new("ESTALE", "Use Remove setup for an installed bridge.")); }
            record.operation.state = OperationState::Cancelled; return self.save(&mut record);
        }
        self.rollback(&mut record, None)
    }
    /// Run once before the service accepts clients after starting or rebooting.
    /// An unconfirmed conversion cannot retain a deadline from an earlier
    /// helper lifetime: its clock and platform checkpoint may no longer exist.
    pub fn recover_startup(&mut self) -> Result<Option<Operation>, BridgeError> {
        self.recover_inner(None)
    }
    /// Periodic recovery in a running helper. Caller identity is irrelevant to
    /// restoration; the service must call recover_startup before serving IPC.
    pub fn recover(&mut self, now: u64) -> Result<Option<Operation>, BridgeError> {
        self.recover_inner(Some(now))
    }
    fn recover_inner(&mut self, now: Option<u64>) -> Result<Option<Operation>, BridgeError> {
        let _lock = self.journal.lock()?;
        let Some(mut record) = self.record()? else { return Ok(None); };
        // A recorded conflict needs an explicit owner retry. Restarting the
        // service must not repeat a restoration the OS already refused.
        if record.undo_required && record.operation.state == OperationState::Failed
            && record.operation.error.as_ref().is_some_and(|error| error.code == "EROLLBACK") {
            return Ok(Some(record.operation));
        }
        let waiting = record.operation.state == OperationState::Verifying
            && now.is_some_and(|now| record.operation.confirmation_deadline.is_some_and(|deadline| now < deadline));
        if record.undo_required && !waiting { return self.rollback(&mut record, None).map(Some); }
        Ok(Some(record.operation))
    }
    fn rollback(&mut self, record: &mut Record<P::Snapshot, P::Mutation>, cause: Option<BridgeError>) -> Result<Operation, BridgeError> {
        record.operation.state = OperationState::RollingBack; record.operation.stage = "Restore previous connection".into();
        record.operation.confirmation_deadline = None; self.save(record)?;
        match self.platform.rollback(&record.prepared.snapshot) {
            Ok(()) => { record.undo_required = false; record.operation.state = if cause.is_some() { OperationState::Failed } else { OperationState::Cancelled }; record.operation.error = cause; },
            Err(error) => { record.operation.state = OperationState::Failed;
                record.operation.error = Some(BridgeError { code: "EROLLBACK".into(), message: "Could not restore the previous connection. Repair is required.".into(), detail: Some(error.to_string()) }); },
        }
        self.save(record)
    }
}
