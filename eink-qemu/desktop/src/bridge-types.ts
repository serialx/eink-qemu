/** Wire contract for native bridge setup. Kept in sync with eink-bridge/model.rs. */
export type BridgePlatform = 'macos' | 'linux' | 'windows';
export type BridgeReadiness = 'needsSetup' | 'ready' | 'inUse' | 'disconnected' | 'needsRepair' | 'unsupported';
export interface BridgeError { code: string; message: string; detail?: string }
export interface BridgeAdapter {
  id: string; name: string; kind: 'ethernet' | 'wifi' | 'bridge' | 'tap' | 'other';
  linkUp: boolean; eligible: boolean; reason: string | null; fingerprint: string;
}
export interface BridgeSelection {
  version: number; platform: BridgePlatform; adapterId: string; fingerprint: string;
  resourceId: string | null; displayName: string;
}
export type BridgePlanRequest = { kind: 'configure'; adapterId: string } | { kind: 'repair' | 'remove'; resourceId: string };
export interface BridgePlan {
  id: string; request: BridgePlanRequest; fingerprint: string; changes: string[];
  requiresAuthorization: boolean; mayDisconnect: boolean; expiresAt: number;
}
export interface BridgeOperation {
  id: string; revision: number;
  state: 'inspecting' | 'awaitingAuthorization' | 'applying' | 'verifying' | 'finalizing' | 'ready' | 'rollingBack' | 'cancelled' | 'failed';
  stage: string; confirmationDeadline: number | null; rebootRequired: boolean; error: BridgeError | null;
}
export interface BridgeInspection {
  support: 'missing' | 'ready' | 'needsUpdate';
  platform: BridgePlatform; adapters: BridgeAdapter[];
  capabilities: { create: boolean; existingBridge: boolean; reason: string | null };
  selection: BridgeSelection | null; readiness: BridgeReadiness;
  activeOperation: BridgeOperation | null; externalOverride: string | null;
}
export interface BridgeBackend {
  install(): Promise<BridgeInspection>;
  uninstall(): Promise<void>;
  inspect(): Promise<BridgeInspection>;
  plan(request: BridgePlanRequest): Promise<BridgePlan>;
  apply(planId: string): Promise<BridgeOperation>;
  keep(operationId: string): Promise<BridgeOperation>;
  cancel(operationId: string): Promise<BridgeOperation>;
  verify(): Promise<BridgeInspection>;
  watch(handler: (operation: BridgeOperation) => void): Promise<() => void>;
}
