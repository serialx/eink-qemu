import type { BridgeError, BridgeInspection, BridgeOperation, BridgePlan } from './bridge-types.ts';

export interface BridgeUiState {
  step: 'support' | 'choose' | 'review' | 'progress' | 'saving' | 'ready' | 'reboot' | 'failed';
  inspection: BridgeInspection | null;
  adapterId: string | null;
  plan: BridgePlan | null;
  operation: BridgeOperation | null;
  error: BridgeError | null;
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
  | { type: 'failed'; error: BridgeError };
export function initialBridgeState(): BridgeUiState {
  return { step: 'choose', inspection: null, adapterId: null, plan: null, operation: null, error: null };
}
const active = (state: BridgeUiState) => ['progress', 'saving', 'reboot'].includes(state.step);

export function reduceBridge(state: BridgeUiState, action: BridgeAction): BridgeUiState {
  switch (action.type) {
    case 'inspected': {
      const next = { ...state, inspection: action.inspection };
      const operation = action.inspection.activeOperation;
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
      if (state.step !== 'review' || !state.plan) return state;
      return { ...state, step: 'progress', error: null };
    case 'operation':
      if (action.operation.id !== (state.operation?.id ?? state.plan?.id)) return state;
      if (state.operation && action.operation.revision <= state.operation.revision) return state;
      return operationState(state, action.operation);
    case 'saved':
      return state.step === 'saving' ? { ...state, step: 'ready' } : state;
    case 'retrySave':
      return state.step === 'failed' && state.operation?.state === 'ready'
        ? { ...state, step: 'saving', error: null } : state;
    case 'failed':
      return { ...state, step: 'failed', error: action.error };
    case 'back':
      return active(state) ? state : { ...initialBridgeState(), inspection: state.inspection,
        adapterId: state.adapterId, operation: state.operation };
  }
}
function operationState(state: BridgeUiState, operation: BridgeOperation): BridgeUiState {
  const next = { ...state, operation, error: operation.error };
  if (operation.state === 'failed') return { ...next, step: 'failed' };
  if (operation.state === 'cancelled') return { ...next, plan: null, step: 'choose' };
  if (operation.rebootRequired) return { ...next, step: 'reboot' };
  if (operation.state === 'ready') return { ...next, step: 'saving' };
  return { ...next, step: 'progress' };
}
export function canApply(state: BridgeUiState, now: number, emulatorIdle: boolean): boolean {
  const { plan, inspection } = state;
  if (!emulatorIdle || state.step !== 'review' || !plan || !inspection || inspection.support !== 'ready' || now >= plan.expiresAt) return false;
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
