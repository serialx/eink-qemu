//! Durable transaction engine. The authenticated helper owns this coordinator
//! and calls recover at startup and on a timer, independently of the WebView.
use crate::{journal::Journal, model::*};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::Path;

const RECORD_VERSION: u32 = 1;
const CLEANUP_RECORD_VERSION: u32 = 2;
const REBOOT_RECORD_VERSION: u32 = 3;
const CANCELLABLE_RECORD_VERSION: u32 = 4;

pub trait PlatformOps {
    type Snapshot: Serialize + DeserializeOwned + Clone;
    type Mutation: Serialize + DeserializeOwned + Clone;
    fn prepare(&mut self, request: PlanRequest, id: OperationId, now: u64, existing: Option<&Resource<Self::Snapshot>>) -> Result<Prepared<Self::Snapshot, Self::Mutation>, BridgeError>;
    fn fingerprint(&mut self, request: &PlanRequest, snapshot: &Self::Snapshot) -> Result<String, BridgeError>;
    fn apply(&mut self, mutation: &Self::Mutation) -> Result<(), BridgeError>;
    /// User-facing progress, saved before entering a potentially slow native call.
    fn mutation_stage(&self, _: &Self::Mutation) -> &'static str { "Configure bridge" }
    /// Record native handles allocated during a step (for example an NM
    /// checkpoint). Preserve the original undo state. The coordinator persists
    /// this context before invoking the next external mutation. A step that
    /// allocates a handle must remain recoverable if it crashes before return.
    fn apply_recorded(&mut self, mutation: &Self::Mutation, _: &mut Self::Snapshot) -> Result<(), BridgeError> { self.apply(mutation) }
    /// Pause only after a nondisruptive prerequisite, before changing host
    /// membership. The platform records the current boot identity in Snapshot.
    fn awaiting_reboot(&self, _: &Self::Snapshot) -> bool { false }
    /// Explicit owner Apply, with fresh OS authorization, resumes the remaining
    /// steps. Require a different boot and validate the installed prerequisite.
    /// Startup and the watchdog must never continue a disruptive conversion.
    fn resume_after_reboot(&mut self, _: &mut Self::Snapshot) -> Result<(), BridgeError> {
        Err(BridgeError::new("EREBOOT", "Restart the computer before continuing bridge setup."))
    }
    fn verify(&mut self, snapshot: &Self::Snapshot) -> Result<(), BridgeError>;
    /// Some transports add temporary native topology while in use. Without a
    /// reliable ownership tag for those members, verify only when idle rather
    /// than treating them as damage or allowing arbitrary foreign interfaces.
    fn requires_idle_transport(&self, _: &Self::Snapshot) -> bool { false }
    /// Arm the native fail-safe after initial verification, immediately before
    /// publishing the user's confirmation deadline. Never resurrect a checkpoint
    /// the OS has already rolled back.
    fn begin_confirmation(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
    /// The owner chose Keep before the visible deadline. Give native verification
    /// and commit their own bounded fail-safe window; retain durable undo state.
    fn begin_keep(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
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
    /// Opt in when confirmed removal has irreversible work, such as deleting
    /// a Windows device whose network GUID cannot be recreated. The coordinator
    /// persists confirmation before cleanup and blocks leases until it finishes.
    fn needs_committed_cleanup(&self, _: &Self::Snapshot) -> bool { false }
    /// Idempotent, conflict-aware cleanup after durable confirmation. Never
    /// restore the old snapshot after this starts. A known failure requires an
    /// explicit owner retry; a process interruption resumes cleanup at startup.
    fn cleanup_committed(&mut self, _: &Self::Snapshot) -> Result<(), BridgeError> { Ok(()) }
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
    cleanup_pending: bool,
    #[serde(default)]
    completed_mutations: usize,
    #[serde(default)]
    in_flight_mutation: Option<usize>,
    resources: Vec<Resource<S>>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancellation { version: u32, owner: String, id: OperationId }
pub struct Coordinator<P: PlatformOps> { journal: Journal, owner: String, platform: P }
impl<P: PlatformOps> Coordinator<P> {
    /// owner must be the authenticated OS principal, never a request field.
    pub fn open(root: &Path, owner: &str, platform: P) -> Result<Self, BridgeError> {
        if owner.is_empty() { return Err(BridgeError::new("EPERM", "Missing bridge caller identity.")); }
        Ok(Self { journal: Journal::open(root)?, owner: owner.into(), platform })
    }
    fn record(&self) -> Result<Option<Record<P::Snapshot, P::Mutation>>, BridgeError> {
        let record: Option<Record<P::Snapshot, P::Mutation>> = self.journal.read()?;
        if record.as_ref().is_some_and(|r| {
            let legacy = if r.cleanup_pending { CLEANUP_RECORD_VERSION }
                else if r.operation.reboot_required { REBOOT_RECORD_VERSION } else { RECORD_VERSION };
            r.version != legacy && !(r.version == CANCELLABLE_RECORD_VERSION && !r.cleanup_pending
                && (r.undo_required || r.operation.state == OperationState::AwaitingAuthorization))
        }) { return Err(BridgeError::new("EPROTOCOL", "Bridge recovery record needs a compatible helper.")); }
        Ok(record)
    }
    fn owned(&self, id: &OperationId) -> Result<Record<P::Snapshot, P::Mutation>, BridgeError> {
        let record = self.record()?.ok_or_else(|| BridgeError::new("ENOENT", "Bridge operation no longer exists."))?;
        if record.owner != self.owner { return Err(BridgeError::new("EPERM", "This bridge operation belongs to another user.")); }
        if record.operation.id != *id { return Err(BridgeError::new("ESTALE", "Bridge operation has changed.")); }
        Ok(record)
    }
    fn save(&self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        // Older helpers must not ignore a queued cancellation or resume an
        // irreversible cleanup. Completed records keep their existing format.
        record.version = if record.cleanup_pending { CLEANUP_RECORD_VERSION }
            else if record.undo_required || record.operation.state == OperationState::AwaitingAuthorization { CANCELLABLE_RECORD_VERSION }
            else if record.operation.reboot_required { REBOOT_RECORD_VERSION } else { RECORD_VERSION };
        record.operation.revision += 1; self.journal.write(record)?; Ok(record.operation.clone())
    }
    pub fn resources(&self) -> Result<Vec<Resource<P::Snapshot>>, BridgeError> {
        // Inspection must remain available while a native request owns the
        // mutation lock. This is metadata from the last atomic journal write;
        // planning and transport acquisition still lock and revalidate it.
        Ok(self.record()?.map(|r| r.resources.into_iter().filter(|r| r.owner == self.owner).collect()).unwrap_or_default())
    }
    /// Privileged package removal only, with the service stopped and its
    /// installation lock held for this entire call. Never expose this through
    /// IPC: removal acts on all recorded owners. The factory must validate each
    /// protected owner against its native snapshot before constructing a backend.
    /// Keep the journal and package installed if any recovery/removal fails.
    pub fn uninstall<F,C>(&mut self,mut for_resource:F,clock:C)->Result<(),BridgeError>
    where F:FnMut(&Resource<P::Snapshot>)->Result<P,BridgeError>,C:Fn()->u64 {
        self.recover_startup()?;
        let resources={
            let _lock=self.journal.lock()?;
            let resources=match self.record()? {
                Some(record) if record.undo_required || record.cleanup_pending=>return Err(record.operation.error
                    .unwrap_or_else(||BridgeError::new("ERECOVERY","Finish the interrupted bridge operation before uninstalling support."))),
                Some(record)=>record.resources,
                None=>Vec::new(),
            };
            // Check every live emulator before removing any resource. The
            // caller's service lock prevents new IPC acquisitions afterward.
            let _leases=resources.iter().map(|r|self.journal.resource_lock(&r.id,false)).collect::<Result<Vec<_>,_>>()?;
            resources
        };
        for resource in resources {
            self.platform=for_resource(&resource)?;
            self.owner=resource.owner;
            let plan=self.plan(PlanRequest::Remove{resource_id:resource.id},clock())?;
            let mut operation=self.apply(&plan.id,clock())?;
            if operation.state==OperationState::Verifying {operation=self.keep(&plan.id,clock())?;}
            if operation.state!=OperationState::Ready || operation.error.is_some() {
                return Err(operation.error.unwrap_or_else(||BridgeError::new("EREMOVE","Bridge removal did not finish. Keep support installed and review the connection.")));
            }
        }
        Ok(())
    }
    /// Service startup only, after interrupted transactions have been restored.
    /// A surviving emulator owns its runtime state until its lease is released.
    /// Per-resource failures remain available for explicit repair and must not
    /// prevent other users' healthy bridges from starting.
    pub fn resume_resources(&mut self) -> Result<Vec<(ResourceId, BridgeError)>, BridgeError> {
        let _lock = self.journal.lock()?;
        let Some(record) = self.record()? else { return Ok(Vec::new()); };
        if record.undo_required && record.operation.reboot_required { return Ok(Vec::new()); }
        if record.undo_required || record.cleanup_pending { return Err(BridgeError::new("ERECOVERY", "Finish the interrupted bridge operation first.")); }
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
        // Journal replacement is atomic. Read the last complete status while
        // a native operation owns the mutation lock, so progress remains
        // available during slow driver/network calls. Mutating decisions still
        // acquire the lock and revalidate their protected record.
        Ok(self.record()?.filter(|record| record.owner == self.owner).map(|record| record.operation))
    }
    /// Bind native approval to the exact protected review and operation
    /// revision. This does not authorize Apply or bypass its live host checks.
    pub fn reviewed_plan(&self, id: &OperationId, now: u64) -> Result<(BridgePlan, u64), BridgeError> {
        let _lock = self.journal.lock()?;
        let record = self.owned(id)?;
        let resuming = record.operation.reboot_required && record.operation.state == OperationState::Applying
            && record.undo_required && record.in_flight_mutation.is_none() && record.completed_mutations > 0;
        if !resuming && (record.operation.state != OperationState::AwaitingAuthorization || now >= record.prepared.plan.expires_at) {
            return Err(BridgeError::new("ESTALE", "Review the current bridge plan before approving it."));
        }
        Ok((record.prepared.plan, record.operation.revision))
    }
    /// Verify while holding both the transaction lock and the transport lease.
    /// The returned lease prevents repair/removal until the native client exits.
    pub fn verify_resource(&mut self, id: &ResourceId) -> Result<(Resource<P::Snapshot>, ResourceLease), BridgeError> {
        let _lock = self.journal.lock()?;
        let record = self.record()?.ok_or_else(|| BridgeError::new("ENOENT", "Bridge setup no longer exists."))?;
        let resource = self.resource(&record.resources, id)?.clone();
        if record.undo_required || record.cleanup_pending { return Err(BridgeError::new("EBUSY", "Finish bridge setup before starting an emulator.")); }
        if self.platform.requires_idle_transport(&resource.original) {
            match self.journal.resource_lock(id, false) {
                Ok(probe) => drop(probe),
                Err(error) if error.code == "EBUSY" => return Err(BridgeError::new("EINUSE", "This bridge is in use. Stop its emulator before verifying it or starting another one.")),
                Err(error) => return Err(error),
            }
        }
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
        if record.undo_required || record.cleanup_pending { return Err(BridgeError::new("EBUSY", "Finish bridge setup before starting an emulator.")); }
        Ok(ResourceLease { _file: self.journal.resource_lock(id, true)? })
    }
    pub fn plan(&mut self, request: PlanRequest, now: u64) -> Result<BridgePlan, BridgeError> {
        let _lock = self.journal.lock()?;
        let mut resources = Vec::new();
        if let Some(record) = self.record()? {
            if record.cleanup_pending { return Err(BridgeError::new("ERECOVERY", "Finish the confirmed bridge cleanup before starting another operation.")); }
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
        let record = Record { version: CANCELLABLE_RECORD_VERSION, owner: self.owner.clone(), prepared, undo_required: false, cleanup_pending: false, resources,
            completed_mutations: 0, in_flight_mutation: None,
            operation: Operation { id, revision: 1, state: OperationState::AwaitingAuthorization,
                stage: "Review bridge setup".into(), confirmation_deadline: None, reboot_required: false, error: None } };
        self.journal.write(&record)?; Ok(plan)
    }
    pub fn apply(&mut self, id: &OperationId, now: u64) -> Result<Operation, BridgeError> {
        self.apply_review(id, now, false)
    }
    /// Call only after verifying fresh, single-use native authorization. If its
    /// prompt outlived the review, rebuild the entire protected plan under the
    /// mutation and resource locks. Only the expiry may differ; a changed
    /// snapshot, mutation, or displayed change requires another user review.
    pub fn apply_after_authorization(&mut self, id: &OperationId, now: u64) -> Result<Operation, BridgeError> {
        self.apply_review(id, now, true)
    }
    fn apply_review(&mut self, id: &OperationId, now: u64, refresh_expired: bool) -> Result<Operation, BridgeError> {
        let started = std::time::Instant::now();
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        let _lease = match &record.prepared.plan.request {
            PlanRequest::Configure { .. } => None,
            PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } => {
                self.resource(&record.resources, resource_id)?;
                Some(self.journal.resource_lock(resource_id, false)?)
            },
        };
        let resuming = record.operation.reboot_required && record.operation.state == OperationState::Applying
            && record.undo_required && record.in_flight_mutation.is_none() && record.completed_mutations > 0;
        if !resuming && record.operation.state != OperationState::AwaitingAuthorization { return Err(BridgeError::new("EBUSY", "Bridge operation has already started.")); }
        if self.cancellation_requested(&record)? { return self.cancel_record(&mut record); }
        if !resuming && now >= record.prepared.plan.expires_at {
            if !refresh_expired {
                return Err(BridgeError::new("ESTALE", "The setup review expired. Review setup again."));
            }
            let existing = match &record.prepared.plan.request {
                PlanRequest::Configure { .. } => None,
                PlanRequest::Repair { resource_id } | PlanRequest::Remove { resource_id } =>
                    Some(self.resource(&record.resources, resource_id)?),
            };
            let mut fresh = self.platform.prepare(record.prepared.plan.request.clone(), id.clone(), now, existing)?;
            let expires_at = fresh.plan.expires_at;
            fresh.plan.expires_at = record.prepared.plan.expires_at;
            let encode = |prepared: &Prepared<P::Snapshot, P::Mutation>| serde_json::to_value(prepared)
                .map_err(|_| BridgeError::new("EPROTOCOL", "Cannot compare the reviewed bridge changes."));
            if expires_at <= now || encode(&fresh)? != encode(&record.prepared)? {
                return Err(BridgeError::new("ESTALE", "The connection or proposed changes changed while awaiting authorization. Review setup again."));
            }
            record.prepared.plan.expires_at = expires_at;
        }
        if self.platform.fingerprint(&record.prepared.plan.request, &record.prepared.snapshot)? != record.prepared.plan.fingerprint {
            return Err(BridgeError::new("ESTALE", "The selected connection changed. Review setup again."));
        }
        if resuming {
            self.platform.resume_after_reboot(&mut record.prepared.snapshot)?;
            if self.platform.awaiting_reboot(&record.prepared.snapshot) { return Err(BridgeError::new("EREBOOT", "Restart the computer before continuing bridge setup.")); }
            record.operation.reboot_required = false;
        }
        record.undo_required = true; record.operation.state = OperationState::Applying;
        record.operation.stage = "Configure bridge".into();
        // Persist restoration data before the first external side effect.
        self.save(&mut record)?;
        for index in record.completed_mutations..record.prepared.mutations.len() {
            if self.cancellation_requested(&record)? { return self.rollback(&mut record, None); }
            record.in_flight_mutation = Some(index);
            record.operation.stage = self.platform.mutation_stage(&record.prepared.mutations[index]).into();
            self.save(&mut record)?;
            if let Err(error) = self.platform.apply_recorded(&record.prepared.mutations[index], &mut record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
            // A crash before this durable completion leaves the step marked
            // uncertain. Recovery restores the snapshot rather than replaying
            // an external action that may already have succeeded.
            record.completed_mutations = index + 1;
            record.in_flight_mutation = None;
            if self.cancellation_requested(&record)? { return self.rollback(&mut record, None); }
            if self.platform.awaiting_reboot(&record.prepared.snapshot) {
                record.operation.reboot_required = true;
                record.operation.stage = "Restart the computer, then continue bridge setup".into();
                return self.save(&mut record);
            }
            self.save(&mut record)?;
        }
        record.operation.state = OperationState::Verifying; record.operation.stage = "Check connection".into();
        self.save(&mut record)?;
        if let Err(error) = self.platform.verify(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
        if self.cancellation_requested(&record)? { return self.rollback(&mut record, None); }
        if record.prepared.plan.may_disconnect {
            if let Err(error) = self.platform.begin_confirmation(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
            if self.cancellation_requested(&record)? { return self.rollback(&mut record, None); }
            record.operation.confirmation_deadline = Some(now.saturating_add(started.elapsed().as_secs()).saturating_add(120));
        }
        else { return self.finish(&mut record); }
        self.save(&mut record)
    }
    pub fn keep(&mut self, id: &OperationId, now: u64) -> Result<Operation, BridgeError> {
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        if record.operation.state != OperationState::Verifying || record.operation.reboot_required || record.operation.confirmation_deadline.is_none() {
            return Err(BridgeError::new("ESTALE", "This bridge operation is not waiting for confirmation."));
        }
        if now >= record.operation.confirmation_deadline.unwrap() { return self.rollback(&mut record, None); }
        if self.cancellation_requested(&record)? { return self.rollback(&mut record, None); }
        if let Err(error) = self.platform.begin_keep(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
        if let Err(error) = self.platform.verify(&record.prepared.snapshot) { return self.rollback(&mut record, Some(error)); }
        self.finish(&mut record)
    }
    fn finish(&mut self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        {
            // Linearize Cancel against the start of finalization. Native commit
            // remains outside this fence so cancellation never blocks on it.
            let fence = self.journal.cancellation_lock()?;
            if self.cancellation_requested(record)? {
                drop(fence);
                return self.rollback(record, None);
            }
            record.operation.state = OperationState::Finalizing;
            record.operation.confirmation_deadline = None;
            record.operation.stage = "Finalize bridge setup".into();
            self.save(record)?;
        }
        if let Err(error) = self.platform.commit_recorded(&mut record.prepared.snapshot) {
            return self.rollback(record, Some(error));
        }
        if self.platform.needs_committed_cleanup(&record.prepared.snapshot) {
            record.undo_required = false;
            record.cleanup_pending = true;
            record.operation.state = OperationState::Finalizing;
            // Once this write succeeds, a restart completes removal. It must
            // never recreate an already deleted device from an undo snapshot.
            self.save(record)?;
            return self.finish_cleanup(record);
        }
        self.commit_resource(record)?;
        record.operation.stage = "Bridge ready".into();
        self.save(record)
    }
    fn finish_cleanup(&mut self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        record.operation.state = OperationState::Finalizing;
        record.operation.stage = "Finish confirmed bridge cleanup".into();
        record.operation.error = None;
        self.save(record)?;
        if let Err(error) = self.platform.cleanup_committed(&record.prepared.snapshot) {
            record.operation.state = OperationState::Failed;
            record.operation.error = Some(BridgeError { code: "ECLEANUP".into(),
                message: "The confirmed network changes are saved, but bridge cleanup needs attention. Retry recovery to finish removal.".into(),
                detail: Some(error.diagnostic()) });
            return self.save(record);
        }
        self.commit_resource(record)?;
        record.cleanup_pending = false;
        record.operation.stage = "Bridge cleanup complete".into();
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
        {
            let _fence = self.journal.cancellation_lock()?;
            let record = self.owned(id)?;
            if !record.cleanup_pending {
                if matches!(record.operation.state, OperationState::Ready | OperationState::Finalizing) {
                    return Err(BridgeError::new("ESTALE", "Bridge setup is already being finalized. Use Remove setup after it finishes."));
                }
                if record.operation.state == OperationState::Cancelled { return Ok(record.operation); }
                // A separate atomic record lets the authenticated owner queue
                // cancellation while Apply owns the mutation lock. EBUSY below
                // means restoration is pending, not that this intent was lost.
                self.journal.write_cancellation(&Cancellation { version: 1, owner: record.owner, id: id.clone() })?;
            }
        }
        let _lock = self.journal.lock()?; let mut record = self.owned(id)?;
        // The existing recovery action retries confirmed cleanup; it cannot
        // cancel changes whose irreversible phase has already started.
        if record.cleanup_pending { return self.finish_cleanup(&mut record); }
        self.cancel_record(&mut record)
    }
    fn cancellation_requested(&self, record: &Record<P::Snapshot, P::Mutation>) -> Result<bool, BridgeError> {
        let Some(intent): Option<Cancellation> = self.journal.read_cancellation()? else { return Ok(false); };
        if intent.version != 1 { return Err(BridgeError::new("EPROTOCOL", "Bridge cancellation needs a compatible helper.")); }
        Ok(intent.id == record.operation.id && intent.owner == record.owner)
    }
    fn cancel_record(&mut self, record: &mut Record<P::Snapshot, P::Mutation>) -> Result<Operation, BridgeError> {
        if record.operation.state == OperationState::Ready {
            return Err(BridgeError::new("ESTALE", "Use Remove setup for an installed bridge."));
        }
        if record.operation.state == OperationState::Cancelled { return Ok(record.operation.clone()); }
        if !record.undo_required {
            record.operation.state = OperationState::Cancelled;
            return self.save(record);
        }
        self.rollback(record, None)
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
        // Atomic journal reads need no transaction lock. Most watchdog ticks
        // have nothing to change; taking the exclusive lock for those reads
        // can reject an approved Apply or Keep racing the tick. A stale no-op
        // observation is harmless: the next tick reads the latest record.
        // Any restoration or cancellation still locks and rereads below.
        let Some(record) = self.record()? else { return Ok(None); };
        let waiting = record.operation.state == OperationState::Verifying
            && record.operation.confirmation_deadline.is_some_and(|deadline| now < deadline);
        let awaiting_reboot = record.operation.state == OperationState::Applying && record.operation.reboot_required;
        if !record.cleanup_pending && (!record.undo_required || waiting || awaiting_reboot)
            && !((record.undo_required || record.operation.state == OperationState::AwaitingAuthorization)
                && self.cancellation_requested(&record)?) {
            return Ok(Some(record.operation));
        }
        self.recover_inner(Some(now))
    }
    fn recover_inner(&mut self, now: Option<u64>) -> Result<Option<Operation>, BridgeError> {
        let _lock = self.journal.lock()?;
        let Some(mut record) = self.record()? else { return Ok(None); };
        if record.cleanup_pending {
            if record.operation.state == OperationState::Failed
                && record.operation.error.as_ref().is_some_and(|error| error.code == "ECLEANUP") {
                return Ok(Some(record.operation));
            }
            return self.finish_cleanup(&mut record).map(Some);
        }
        // A recorded conflict needs an explicit owner retry. Restarting the
        // service must not repeat a restoration the OS already refused.
        if record.undo_required && record.operation.state == OperationState::Failed
            && record.operation.error.as_ref().is_some_and(|error| error.code == "EROLLBACK") {
            return Ok(Some(record.operation));
        }
        if (record.undo_required || record.operation.state == OperationState::AwaitingAuthorization)
            && self.cancellation_requested(&record)? { return self.cancel_record(&mut record).map(Some); }
        if record.undo_required && record.operation.reboot_required && record.operation.state == OperationState::Applying {
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
            Ok(()) => { record.undo_required = false; record.operation.reboot_required = false; record.operation.state = if cause.is_some() { OperationState::Failed } else { OperationState::Cancelled }; record.operation.error = cause; },
            Err(error) => { record.operation.state = OperationState::Failed;
                record.operation.error = Some(BridgeError { code: "EROLLBACK".into(), message: "Could not restore the previous connection. Repair is required.".into(), detail: Some(error.diagnostic()) }); },
        }
        self.save(record)
    }
}
