import type { BridgeError, BridgeInspection, BridgeOperation, BridgePlan } from './bridge-types.ts';

export interface BridgeUiState {
  step: 'support' | 'choose' | 'review' | 'progress' | 'saving' | 'ready' | 'reboot' | 'failed' | 'reviewUninstall' | 'uninstalling' | 'uninstalled';
  inspection: BridgeInspection | null;
  adapterId: string | null;
  plan: BridgePlan | null;
  operation: BridgeOperation | null;
  error: BridgeError | null;
  saveFailed: boolean;
}
export type BridgeAction =
  | { type: 'inspected'; inspection: BridgeInspection }
  | { type: 'select'; adapterId: string }
  | { type: 'planned'; plan: BridgePlan }
  | { type: 'applying' }
  | { type: 'operation'; operation: BridgeOperation }
  | { type: 'saved' }
  | { type: 'retrySave' }
  | { type: 'back' }
  | { type: 'reviewUninstall' }
  | { type: 'uninstalling' }
  | { type: 'uninstalled' }
  | { type: 'failed'; error: BridgeError };
export function initialBridgeState(): BridgeUiState {
  return { step: 'choose', inspection: null, adapterId: null, plan: null, operation: null, error: null, saveFailed: false };
}
const active = (state: BridgeUiState) => ['progress', 'saving', 'reboot', 'uninstalling'].includes(state.step);

export function reduceBridge(state: BridgeUiState, action: BridgeAction): BridgeUiState {
  if (['reviewUninstall', 'uninstalling', 'uninstalled'].includes(state.step)
    && (action.type === 'operation' || action.type === 'inspected')) return state;
  switch (action.type) {
    case 'reviewUninstall':
      return active(state) || state.inspection?.platform !== 'macos' || state.inspection.support !== 'ready'
        ? state : { ...state, step: 'reviewUninstall', error: null };
    case 'uninstalling':
      return state.step === 'reviewUninstall' ? { ...state, step: 'uninstalling' } : state;
    case 'uninstalled':
      return state.step === 'uninstalling' ? { ...initialBridgeState(), step: 'uninstalled' } : state;
    case 'inspected': {
      const next = { ...state, inspection: action.inspection };
      const operation = action.inspection.activeOperation;
      // A historical Ready record cannot trigger fresh verification/saving
      // while another emulator owns the transport. Show its retained setup.
      if (operation?.state === 'ready' && action.inspection.readiness === 'inUse' && !active(state)) {
        return { ...next, operation, step: 'choose', error: null, saveFailed: false };
      }
      if (operation && ((!state.operation && (!state.plan || state.plan.id === operation.id))
        || (state.operation?.id === operation.id && operation.revision > state.operation.revision))) {
        return operationState(next, operation);
      }
      if (state.step === 'choose' || state.step === 'support') {
        return { ...next, step: action.inspection.support === 'ready' ? 'choose' : 'support' };
      }
      return next;
    }
    case 'select':
      if (active(state)) return state;
      return { ...state, adapterId: action.adapterId, plan: null, step: 'choose', error: null };
    case 'planned':
      if (active(state)) return state;
      if (action.plan.request.kind === 'configure' && action.plan.request.adapterId !== state.adapterId) return state;
      return { ...state, plan: action.plan, operation: null, step: 'review', error: null };
    case 'applying':
      if (!(state.step === 'review' && state.plan) && !(state.step === 'reboot' && state.operation?.rebootRequired)) return state;
      return { ...state, step: 'progress', error: null };
    case 'operation':
      if (action.operation.id !== (state.operation?.id ?? state.plan?.id)) return state;
      if (state.operation && action.operation.revision <= state.operation.revision) return state;
      return operationState(state, action.operation);
    case 'saved':
      return state.step === 'saving' ? { ...state,
        step: !state.plan && !state.inspection?.selection ? 'choose' : 'ready' } : state;
    case 'retrySave':
      return state.step === 'failed' && state.saveFailed
        ? { ...state, step: 'saving', error: null, saveFailed: false } : state;
    case 'failed':
      return { ...state, step: 'failed', error: action.error,
        saveFailed: state.step === 'saving' || (state.step === 'failed' && state.saveFailed) };
    case 'back':
      return active(state) ? state : { ...initialBridgeState(), inspection: state.inspection,
        adapterId: state.adapterId, operation: state.operation };
  }
}
/** A reopened completed removal has no plan or selection. Native verification
 * still clears stale settings; it must not require a bridge that was removed. */
export function verifiedSelection(plan: BridgePlan | null, inspection: BridgeInspection): boolean {
  if (inspection.support !== 'ready') return false;
  if (inspection.selection) return inspection.readiness === 'ready';
  return inspection.readiness === 'needsSetup' && (!plan || plan.request.kind === 'remove');
}
function operationState(state: BridgeUiState, operation: BridgeOperation): BridgeUiState {
  const next = { ...state, operation, error: operation.error, saveFailed: false };
  if (operation.state === 'awaitingAuthorization') {
    // Planning creates a journal entry before the user presses Apply. Polling
    // that entry must neither skip review nor re-enable an in-flight Apply.
    return { ...next, step: state.step === 'progress' ? 'progress'
      : state.plan?.id === operation.id ? 'review' : 'choose' };
  }
  if (operation.state === 'failed') return { ...next, step: 'failed' };
  if (operation.state === 'cancelled') return { ...next, plan: null, step: 'choose' };
  if (operation.rebootRequired) return { ...next, step: 'reboot' };
  if (operation.state === 'ready') return { ...next, step: 'saving' };
  return { ...next, step: 'progress' };
}
export function canApply(state: BridgeUiState, now: number, emulatorIdle: boolean): boolean {
  const { plan, inspection } = state;
  if (!emulatorIdle || state.step !== 'review' || !plan || !inspection || inspection.support !== 'ready' || now >= plan.expiresAt) return false;
  if (inspection.readiness === 'inUse' && plan.request.kind !== 'configure') return false;
  if (plan.request.kind === 'remove') return inspection.selection?.resourceId === plan.request.resourceId;
  if (inspection.externalOverride) return false;
  const adapterId = plan.request.kind === 'configure' ? plan.request.adapterId : inspection.selection?.adapterId;
  const adapter = inspection.adapters.find(a => a.id === adapterId);
  return !!adapter && adapter.eligible && adapter.linkUp && adapter.fingerprint === plan.fingerprint;
}
export function canKeep(state: BridgeUiState, now: number): boolean {
  const op = state.operation;
  return !!op && state.step === 'progress' && op.state === 'verifying' && !op.rebootRequired
    && op.confirmationDeadline !== null && now < op.confirmationDeadline;
}
export function canResume(state: BridgeUiState, emulatorIdle: boolean): boolean {
  return emulatorIdle && state.step === 'reboot' && state.operation?.state === 'applying'
    && state.operation.rebootRequired && state.inspection?.support === 'ready' && !state.inspection.externalOverride;
}
