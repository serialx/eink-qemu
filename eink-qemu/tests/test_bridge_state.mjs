import assert from 'node:assert/strict';
import test from 'node:test';
import { initialBridgeState, reduceBridge, canApply, canKeep, verifiedSelection } from '../desktop/src/bridge-state.ts';
const id = '0123456789abcdef0123456789abcdef';
const adapter = { id: 'en7', name: 'USB Ethernet', kind: 'ethernet', linkUp: true, eligible: true, reason: null, fingerprint: 'original' };
const inspection = { support: 'ready', platform: 'macos', adapters: [adapter], capabilities: { create: true, existingBridge: false, reason: null }, selection: null, readiness: 'needsSetup', activeOperation: null, externalOverride: null };
const plan = { id, request: { kind: 'configure', adapterId: 'en7' }, fingerprint: 'original', changes: ['Install bridge support'], requiresAuthorization: true, mayDisconnect: false, expiresAt: 300 };
const operation = (revision, state, extra = {}) => ({ id, revision, state, stage: 'Check connection', confirmationDeadline: null, rebootRequired: false, error: null, ...extra });
const prepared = () => {
  let state = reduceBridge(initialBridgeState(), { type: 'inspected', inspection });
  state = reduceBridge(state, { type: 'select', adapterId: 'en7' });
  return reduceBridge(state, { type: 'planned', plan });
};

test('Apply requires a live adapter, fresh plan, and idle emulator', () => {
  assert.equal(canApply(prepared(), 299, true), true);
  assert.equal(canApply(prepared(), 300, true), false);
  assert.equal(canApply(prepared(), 299, false), false);
  const unplugged = reduceBridge(prepared(), { type: 'inspected', inspection: { ...inspection, adapters: [] } });
  assert.equal(canApply(unplugged, 299, true), false);
  const replaced = reduceBridge(prepared(), { type: 'inspected', inspection: { ...inspection, adapters: [{ ...adapter, fingerprint: 'different' }] } });
  assert.equal(canApply(replaced, 299, true), false);
});

test('starting Apply prevents double submission and ignores old or unrelated operations', () => {
  const applying = reduceBridge(prepared(), { type: 'applying' });
  assert.equal(canApply(applying, 100, true), false);
  const current = reduceBridge(applying, { type: 'operation', operation: operation(3, 'verifying') });
  assert.equal(reduceBridge(current, { type: 'operation', operation: operation(2, 'applying') }), current);
  assert.equal(reduceBridge(current, { type: 'operation', operation: { ...operation(4, 'ready'), id: 'f'.repeat(32) } }), current);
});

test('polling a planned operation keeps review visible until Apply is submitted', () => {
  let state = reduceBridge(prepared(), {type:'operation',operation:operation(1,'awaitingAuthorization')});
  assert.equal(state.step, 'review');
  assert.equal(canApply(state, 100, true), true);
  state = reduceBridge(state, {type:'applying'});
  state = reduceBridge(state, {type:'operation',operation:operation(2,'awaitingAuthorization')});
  assert.equal(state.step, 'progress', 'an outstanding authorization request must not enable double submission');
  const reopened = reduceBridge(initialBridgeState(), {type:'inspected',inspection:{...inspection,activeOperation:operation(1,'awaitingAuthorization')}});
  assert.equal(reopened.step, 'choose', 'an abandoned plan must be reviewed again');
});

test('authorization cancellation and failed restoration are distinct outcomes', () => {
  const applying = reduceBridge(prepared(), { type: 'applying' });
  const cancelled = reduceBridge(applying, { type: 'operation', operation: operation(1, 'cancelled') });
  assert.equal(cancelled.step, 'choose');
  assert.equal(cancelled.plan, null);
  const failed = reduceBridge(applying, { type: 'operation', operation: operation(2, 'failed', { error: { code: 'EROLLBACK', message: 'Restoration failed.' } }) });
  assert.equal(failed.step, 'failed');
  assert.equal(failed.error.code, 'EROLLBACK');
});

test('Keep is offered only during verification, before the deadline, with no pending reboot', () => {
  const verifying = reduceBridge(reduceBridge(prepared(), { type: 'applying' }), { type: 'operation', operation: operation(3, 'verifying', { confirmationDeadline: 120 }) });
  assert.equal(canKeep(verifying, 119), true);
  assert.equal(canKeep(verifying, 120), false);
  const reboot = reduceBridge(verifying, { type: 'operation', operation: operation(4, 'verifying', { confirmationDeadline: 120, rebootRequired: true }) });
  assert.equal(canKeep(reboot, 119), false);
  assert.equal(reboot.step, 'reboot');
});

test('persisting settings must succeed before the UI reports setup ready', () => {
  let state = reduceBridge(reduceBridge(prepared(), { type: 'applying' }), { type: 'operation', operation: operation(5, 'ready') });
  assert.equal(state.step, 'saving');
  state = reduceBridge(state, { type: 'failed', error: { code: 'EIO', message: 'Cannot save settings.' } });
  assert.equal(state.step, 'failed');
  assert.equal(state.operation.state, 'ready'); // Host setup exists and must remain recoverable.
});

test('retrying a settings save preserves the native operation revision', () => {
  let state = reduceBridge(reduceBridge(prepared(), { type:'applying' }), { type:'operation', operation:operation(5,'ready') });
  state = reduceBridge(state,{type:'failed',error:{code:'EIO',message:'Save failed'}});
  const retry = reduceBridge(state,{type:'retrySave'});
  assert.equal(retry.step,'saving');
  assert.equal(retry.operation.revision,5);
  assert.equal(reduceBridge(prepared(),{type:'retrySave'}).step,'review');
});

test('reopening inspection can reconcile a completed reboot operation', () => {
  let state = reduceBridge(reduceBridge(prepared(),{type:'applying'}),{type:'operation',operation:operation(3,'verifying',{rebootRequired:true})});
  state = reduceBridge(state,{type:'inspected',inspection:{...inspection,activeOperation:operation(4,'ready')}});
  assert.equal(state.step,'saving');
  assert.equal(state.operation.revision,4);
  const old = reduceBridge(state,{type:'inspected',inspection:{...inspection,activeOperation:operation(3,'verifying',{rebootRequired:true})}});
  assert.equal(old.operation.revision,4);
});

test('missing support is reviewed before selecting or applying a network change', () => {
  const state = reduceBridge(initialBridgeState(), { type:'inspected', inspection:{...inspection, support:'missing'} });
  assert.equal(state.step, 'support');
  assert.equal(canApply(state, 100, true), false);
  const installed = reduceBridge(state, { type:'inspected', inspection:{...inspection, support:'ready'} });
  assert.equal(installed.step, 'choose');
});

test('Manage setup stays available after reopening a completed operation', () => {
  const completed = {...inspection, selection:{resourceId:id}, readiness:'ready', activeOperation:operation(5, 'ready')};
  let state = reduceBridge(initialBridgeState(), {type:'inspected', inspection:completed});
  state = reduceBridge(state, {type:'saved'});
  assert.equal(state.step, 'ready');
  state = reduceBridge(state, {type:'back'});
  state = reduceBridge(state, {type:'inspected', inspection:completed});
  assert.equal(state.step, 'choose', 'the last completed operation must not reopen the save/ready screen');
});

test('reopening a completed removal clears the saved selection and returns to connection choices', () => {
  const removed = {...inspection, activeOperation:operation(5, 'ready')};
  let state = reduceBridge(initialBridgeState(), {type:'inspected', inspection:removed});
  assert.equal(state.step, 'saving');
  assert.equal(verifiedSelection(state.plan, removed), true);
  state = reduceBridge(state, {type:'saved'});
  assert.equal(state.step, 'choose');
  assert.equal(reduceBridge(state, {type:'inspected', inspection:removed}).step, 'choose');
  assert.equal(verifiedSelection(plan, removed), false, 'configuration still requires a verified selection');
  assert.equal(verifiedSelection({...plan, request:{kind:'repair',resourceId:id}}, removed), false);
  assert.equal(verifiedSelection(null, {...removed, support:'missing'}), false);
  assert.equal(verifiedSelection(null, {...removed, selection:{resourceId:id}, readiness:'needsRepair'}), false);
});
