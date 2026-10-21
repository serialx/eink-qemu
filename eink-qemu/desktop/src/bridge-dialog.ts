import type { BridgeBackend, BridgeError, BridgeOperation, BridgePlanRequest } from './bridge-types.ts';
import { canApply, canKeep, canResume, initialBridgeState, reduceBridge, verifiedSelection } from './bridge-state.ts';
import type { BridgeAction, BridgeUiState } from './bridge-state.ts';
import { dialogOpen, registerDialog } from './dialog.ts';

export function bridgeView(state: BridgeUiState) {
  const view = { title: 'Choose a connection', message: 'Choose the host connection that should carry the emulated device’s network traffic.',
    canClose: !['progress', 'saving'].includes(state.step), retrySave: false,
    canCancel: state.step === 'progress' && !['rollingBack', 'finalizing'].includes(state.operation?.state ?? ''),
    retryRecovery: null as string | null };
  switch (state.step) {
    case 'support':
      view.title = state.inspection?.support === 'needsUpdate' ? 'Update bridge support' : 'Install bridge support';
      view.message = 'Install the networking support used to create, check and restore bridge connections. Your operating system will request administrator authorization.';
      break;
    case 'choose':
      if (state.inspection?.externalOverride) view.message = `${state.inspection.externalOverride} controls networking for this launch. Remove that environment override before configuring a connection here.`;
      break;
    case 'review':
      view.title = state.plan?.request.kind === 'remove' ? 'Review removal' : 'Review bridge setup';
      view.message = 'Review the changes before continuing.';
      break;
    case 'progress':
      view.title = state.operation?.state === 'rollingBack' ? 'Restoring your connection'
        : state.operation?.state === 'finalizing' ? 'Finishing bridge changes' : 'Setting up bridge';
      view.message = state.operation?.stage ?? 'Approve the system authorization prompt to continue.';
      break;
    case 'saving':
      view.title = 'Saving setup';
      view.message = 'Host setup succeeded. Verifying and saving your selection…';
      break;
    case 'ready':
      view.title = state.plan?.request.kind === 'remove' ? 'Bridge setup removed' : 'Bridge ready';
      view.message = state.plan?.request.kind === 'remove'
        ? 'The recorded setup has been removed. Choose a connection to configure another bridge.'
        : 'The host bridge is ready. Start the emulator, then join PICSimLabWifi in the firmware to connect the device.';
      break;
    case 'reboot':
      view.title = 'Restart required';
      view.message = 'Restart the computer, then choose Continue setup to finish configuring the connection.';
      break;
    case 'failed':
      view.title = state.error?.code === 'EROLLBACK' ? 'Connection needs repair'
        : state.error?.code === 'ECLEANUP' ? 'Removal needs attention'
        : state.error?.code === 'EAUTHCANCELLED' ? 'Authorization cancelled' : 'Setup could not finish';
      view.message = state.error?.message ?? 'Check the connection and try again.';
      if (state.operation) view.retryRecovery = state.error?.code === 'EROLLBACK' ? 'Retry restoration'
        : state.error?.code === 'ECLEANUP' ? 'Retry cleanup' : null;
      if (state.operation?.state === 'ready') {
        view.retrySave = true;
        view.message += ' Host setup exists, but the app could not save the selection. Retry saving before making more host changes.';
      }
      break;
  }
  return view;
}

function nativeError(error: unknown): BridgeError {
  if (error && typeof error === 'object' && 'code' in error && 'message' in error) {
    return { code: String(error.code), message: String(error.message) };
  }
  return { code: 'EBRIDGE', message: error instanceof Error ? error.message : String(error) };
}

/** The backend owns authorization, persistence and recovery; the dialog only
 * submits typed intents. Closing the WebView must never disable recovery. */
export async function openBridgeDialog(backend: BridgeBackend, trigger: HTMLElement,
  emulatorIdle: () => boolean = () => true): Promise<void> {
  if (dialogOpen()) return;
  let state = initialBridgeState();
  let disposed = false, busy = false, finishing = false;
  let unwatch: (() => void) | undefined;
  const dialog = document.createElement('dialog');
  dialog.className = 'ask bridge-dialog';
  dialog.setAttribute('aria-labelledby', 'bridge-dialog-title');
  const unregister = registerDialog(dialog);
  const heading = document.createElement('h2');
  heading.id = 'bridge-dialog-title';
  heading.tabIndex = -1;
  const message = document.createElement('p');
  message.setAttribute('role', 'status');
  message.setAttribute('aria-live', 'polite');
  const body = document.createElement('div');
  const actions = document.createElement('div');
  actions.className = 'ask-buttons';
  const countdown = document.createElement('p');
  countdown.className = 'bridge-countdown';
  dialog.append(heading, message, body, countdown, actions);
  document.body.append(dialog);
  const now = () => Math.floor(Date.now() / 1000);
  const send = (action: BridgeAction) => {
    if (disposed) return;
    state = reduceBridge(state, action);
    render();
  };
  const run = async (action: () => Promise<void>) => {
    if (busy || disposed) return;
    busy = true; render();
    try { await action(); }
    catch (error) { send({ type: 'failed', error: nativeError(error) }); }
    finally { busy = false; if (!disposed) render(); }
  };
  const finish = async () => {
    if (finishing || disposed) return;
    finishing = true;
    try {
      const inspection = await backend.verify();
      if (disposed) return;
      if (!verifiedSelection(state.plan, inspection)) {
        throw { code: 'EVERIFY', message: 'The saved connection is not ready. Inspect it again before starting the emulator.' };
      }
      send({ type: 'inspected', inspection });
      send({ type: 'saved' });
    } catch (error) { send({ type: 'failed', error: nativeError(error) }); }
    finally { finishing = false; }
  };
  const receive = (operation: BridgeOperation) => {
    const previous = state;
    send({ type: 'operation', operation });
    if (state !== previous && state.step === 'saving') void finish();
  };
  const plan = (request: BridgePlanRequest) => run(async () => {
    send({ type: 'planned', plan: await backend.plan(request) });
  });
  const refresh = () => run(async () => {
    send({ type: 'inspected', inspection: await backend.inspect() });
    if (state.step === 'saving') await finish();
  });
  const button = (label: string, action: () => void, enabled = true, primary = false) => {
    const element = document.createElement('button');
    element.type = 'button';
    element.textContent = label;
    element.disabled = busy || !enabled;
    if (primary) element.className = 'primary';
    element.addEventListener('click', action);
    actions.append(element);
    return element;
  };
  function render() {
    const focused = document.activeElement as HTMLElement | null;
    const focusKey = focused?.dataset.bridgeFocus;
    const focusLabel = focused?.tagName === 'BUTTON' ? focused.textContent : null;
    const view = bridgeView(state);
    heading.textContent = view.title;
    message.textContent = view.message;
    body.replaceChildren(); actions.replaceChildren();
    if (state.step === 'support') {
      if (!emulatorIdle()) { const note = document.createElement('p'); note.textContent = 'Stop the emulator to install bridge support.'; body.append(note); }
      button(state.inspection?.support === 'needsUpdate' ? 'Update support' : 'Install support', () => void run(async () => {
        const inspection = await backend.install();
        if (inspection.support !== 'ready') throw { code: 'EINSTALL', message: 'Bridge support is not available yet. Check the system authorization prompt and try again.' };
        send({ type: 'inspected', inspection });
      }), emulatorIdle(), true);
      button('Check again', () => void refresh());
    } else if (state.step === 'choose') {
      if (!state.inspection) {
        const loading = document.createElement('p'); loading.textContent = 'Inspecting host connections…'; body.append(loading);
      } else {
        if (state.inspection.selection) {
          const current = document.createElement('p');
          current.textContent = `Current setup: ${state.inspection.selection.displayName}`;
          body.append(current);
        }
        const group = document.createElement('div');
        group.className = 'ask-choices';
        group.setAttribute('role', 'radiogroup');
        group.setAttribute('aria-label', 'Host connections');
        for (const adapter of state.inspection.adapters) {
          const label = document.createElement('label');
          const input = document.createElement('input');
          input.type = 'radio'; input.name = 'bridge-adapter';
          input.value = adapter.id; input.dataset.bridgeFocus = adapter.id;
          input.checked = state.adapterId === adapter.id;
          input.disabled = busy || !adapter.eligible || !adapter.linkUp || !!state.inspection.externalOverride || !emulatorIdle();
          input.addEventListener('change', () => send({ type: 'select', adapterId: adapter.id }));
          const text = document.createElement('span'), name = document.createElement('b'), note = document.createElement('small');
          name.textContent = adapter.name;
          note.textContent = adapter.reason ?? (adapter.linkUp ? adapter.kind === 'bridge' ? 'Existing bridge' : 'Connected' : 'Disconnected');
          text.append(name, note); label.append(input, text); group.append(label);
        }
        body.append(group);
        if (state.inspection.capabilities.reason) {
          const reason = document.createElement('p'); reason.textContent = state.inspection.capabilities.reason; body.append(reason);
        }
        if (!emulatorIdle()) { const note = document.createElement('p'); note.textContent = 'Stop the emulator to change bridge setup.'; body.append(note); }
        const resource = state.inspection.selection?.resourceId;
        if (resource) {
          button('Repair', () => void plan({ kind: 'repair', resourceId: resource }), emulatorIdle() && !state.inspection.externalOverride);
          button('Remove setup', () => void plan({ kind: 'remove', resourceId: resource }), emulatorIdle());
        }
      }
      button('Refresh', () => void refresh());
      const selected = state.inspection?.adapters.find(a => a.id === state.adapterId);
      button('Review setup', () => { if (selected) void plan({ kind: 'configure', adapterId: selected.id }); },
        !!selected?.eligible && selected.linkUp && !state.inspection?.externalOverride && emulatorIdle(), true);
    } else if (state.step === 'review') {
      const list = document.createElement('ul');
      for (const change of state.plan?.changes ?? []) { const item = document.createElement('li'); item.textContent = change; list.append(item); }
      body.append(list);
      if (state.plan?.mayDisconnect) {
        const warning = document.createElement('p');
        warning.textContent = 'Your host connection may briefly disconnect. Confirm Keep connection within two minutes, or setup will restore the previous connection.';
        body.append(warning);
      }
      if (state.plan?.requiresAuthorization) {
        const auth = document.createElement('p'); auth.textContent = 'Your operating system will request administrator authorization.'; body.append(auth);
      }
      button('Back', () => send({ type: 'back' }));
      button(state.plan?.request.kind === 'remove' ? 'Remove setup' : 'Apply setup', () => void run(async () => {
        if (!canApply(state, now(), emulatorIdle())) return;
        const id = state.plan!.id;
        send({ type: 'applying' });
        receive(await backend.apply(id));
      }), canApply(state, now(), emulatorIdle()), true);
    } else if (state.step === 'progress') {
      if (view.canCancel) {
        const id = state.operation?.id ?? state.plan?.id;
        if (id) button(state.operation?.confirmationDeadline ? 'Restore previous connection' : 'Cancel setup',
          () => void run(async () => receive(await backend.cancel(id))));
      }
      if (state.operation?.confirmationDeadline) {
        button('Keep connection', () => void run(async () => {
          if (canKeep(state, now())) receive(await backend.keep(state.operation!.id));
        }), canKeep(state, now()), true);
      }
    } else if (state.step === 'failed') {
      if (view.retryRecovery && state.operation) {
        const id = state.operation.id;
        button(view.retryRecovery, () => void run(async () => receive(await backend.cancel(id))), emulatorIdle(), true);
      }
      if (view.retrySave) button('Retry saving', () => void run(async () => {
        send({ type: 'retrySave' });
        await finish();
      }), true, true);
      else if (!view.retryRecovery) button('Inspect again', () => { send({ type: 'back' }); void refresh(); });
    } else if (state.step === 'ready') {
      button('Manage setup', () => { send({ type: 'back' }); void refresh(); });
    } else if (state.step === 'reboot') {
      if (state.operation) {
        const id = state.operation.id;
        button('Cancel setup', () => void run(async () => receive(await backend.cancel(id))), emulatorIdle());
        button('Continue setup', () => void run(async () => {
          if (!canResume(state, emulatorIdle())) return;
          send({ type: 'applying' });
          receive(await backend.apply(id));
        }), canResume(state, emulatorIdle()), true);
      }
    }
    if (view.canClose) button(state.step === 'ready' ? 'Done' : 'Close', () => dialog.close());
    tick();
    if (focused && dialog.contains(focused)) return;
    const replacement = focusKey
      ? [...body.querySelectorAll<HTMLInputElement>('input')].find(e => e.dataset.bridgeFocus === focusKey)
      : [...actions.querySelectorAll<HTMLButtonElement>('button')].find(e => e.textContent === focusLabel);
    if (replacement && !replacement.disabled) replacement.focus();
    else if (focused && focused !== trigger && focused !== document.body) heading.focus();
  }
  function tick() {
    const deadline = state.operation?.confirmationDeadline;
    countdown.textContent = state.step === 'progress' && deadline !== null && deadline !== undefined
      ? `Automatic restoration in ${Math.max(0, deadline - now())} seconds.` : '';
    countdown.hidden = !countdown.textContent;
    for (const b of actions.querySelectorAll<HTMLButtonElement>('button')) {
      if (b.textContent === 'Keep connection') b.disabled = busy || !canKeep(state, now());
      if (b.textContent === 'Continue setup') b.disabled = busy || !canResume(state, emulatorIdle());
      if (state.step === 'review' && (b.textContent === 'Apply setup' || b.textContent === 'Remove setup')) b.disabled = busy || !canApply(state, now(), emulatorIdle());
    }
  }
  dialog.addEventListener('cancel', event => { if (!bridgeView(state).canClose || busy) event.preventDefault(); });
  render();
  dialog.showModal();
  heading.focus();
  const timer = window.setInterval(tick, 1000);
  const closed = new Promise<void>(resolve => dialog.addEventListener('close', () => {
    disposed = true; window.clearInterval(timer); unwatch?.(); unregister(); dialog.remove();
    if (trigger.isConnected) trigger.focus();
    resolve();
  }, { once: true }));
  try {
    const stop = await backend.watch(receive);
    if (disposed) stop(); else unwatch = stop;
    if (!disposed) await refresh();
  } catch (error) { send({ type: 'failed', error: nativeError(error) }); }
  await closed;
}
