import type { BridgeBackend, BridgeError, BridgeInspection, BridgeOperation, BridgePlanRequest } from './bridge-types.ts';
import { canApply, canKeep, canResume, initialBridgeState, reduceBridge, verifiedSelection } from './bridge-state.ts';
import type { BridgeAction, BridgeUiState } from './bridge-state.ts';
import { dialogOpen, registerDialog } from './dialog.ts';

export function bridgeView(state: BridgeUiState) {
  const view = { title: 'Choose a connection', message: 'Choose the host connection that should carry the emulated device’s network traffic.',
    canInstallSupport: state.inspection?.platform === 'macos',
    canClose: !['progress', 'saving', 'uninstalling'].includes(state.step), retrySave: false,
    canCancel: state.step === 'progress' && !['rollingBack', 'finalizing'].includes(state.operation?.state ?? ''),
    retryRecovery: null as string | null };
  switch (state.step) {
    case 'reviewUninstall':
      view.title = 'Uninstall bridge support?';
      view.message = 'This removes the app’s bridge connections for all accounts and uninstalls its privileged helper. Stop every emulator first. Changed connections are preserved if cleanup cannot finish. Your firmware and SD cards remain available. macOS will request administrator authorization; its approval button may say “Add Helper.”';
      break;
    case 'uninstalling':
      view.title = 'Uninstalling bridge support';
      view.message = 'Approve the macOS prompt, which may say “Add Helper,” to authorize removal. The helper checks for running emulators and restores recorded connections before removing support.';
      break;
    case 'uninstalled':
      view.title = 'Bridge support uninstalled';
      view.message = 'The bridge connections and privileged helper have been removed. You can install support again from bridge setup. The protected recovery record is retained.';
      break;
    case 'support':
      view.title = state.inspection?.support === 'needsUpdate' ? 'Update bridge support' : 'Install bridge support';
      view.message = view.canInstallSupport
        ? 'Install the networking support used to create, check and restore bridge connections. Your operating system will request administrator authorization.'
        : state.inspection?.platform === 'linux'
          ? 'Bridge support is included in the eink-qemu Debian (.deb) package. Reinstall or update that package using your software installer, then choose Check again.'
          : 'Bridge support is included in the eink-qemu Windows installer. Run the installer again to repair or update the app, then choose Check again.';
      break;
    case 'choose':
      if (state.inspection?.readiness === 'inUse') view.message = 'This bridge is in use. Stop its emulator before checking or changing the connection. Its current network configuration has not been reverified.';
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
      if (state.saveFailed) {
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
  let applying = false, cancelling = false;
  let cancelId: string | null = null;
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
    if (busy || disposed || cancelId) return;
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
    if (cancelId === operation.id && (['cancelled', 'ready'].includes(operation.state)
      || (operation.state === 'failed' && ['EROLLBACK', 'ECLEANUP'].includes(operation.error?.code ?? '')))) cancelId = null;
    const previous = state;
    send({ type: 'operation', operation });
    if (state !== previous && state.step === 'saving') void finish();
  };
  const attemptCancel = async () => {
    if (!cancelId || cancelling || disposed) return;
    const id = cancelId;
    cancelling = true;
    try {
      const operation = await backend.cancel(id);
      if (disposed || cancelId !== id) return;
      cancelId = null;
      receive(operation);
    } catch (error) {
      if (disposed || cancelId !== id) return;
      const failure = nativeError(error);
      // The helper holds its journal lock during a native step. Keep the
      // user's cancellation request and retry after that bounded work ends.
      if (failure.code !== 'EBUSY') { cancelId = null; send({ type: 'failed', error: failure }); }
    } finally { cancelling = false; if (!disposed) render(); }
  };
  const cancel = (id: string) => {
    if (cancelId || disposed || (busy && !applying) || !bridgeView(state).canCancel
      || id !== (state.operation?.id ?? state.plan?.id)) return;
    cancelId = id; render(); void attemptCancel();
  };
  const apply = async (id: string) => {
    applying = true;
    send({ type: 'applying' });
    try { receive(await backend.apply(id)); }
    catch (error) {
      // A cancellation during the OS prompt invalidates the approval. Its late
      // rejection must not replace the already-confirmed cancelled state.
      if (state.operation?.id !== id || state.operation.state !== 'cancelled') throw error;
    } finally { applying = false; }
  };
  const plan = (request: BridgePlanRequest) => run(async () => {
    send({ type: 'planned', plan: await backend.plan(request) });
  });
  const inspectResult = async (inspection: BridgeInspection) => {
    send({ type: 'inspected', inspection });
    if (state.step === 'saving') await finish();
  };
  const refresh = () => run(async () => {
    await inspectResult(await backend.inspect());
  });
  const button = (label: string, action: () => void, enabled = true, primary = false, duringApply = false) => {
    const element = document.createElement('button');
    element.type = 'button';
    element.textContent = label;
    element.disabled = (busy && !(duringApply && applying)) || cancelId !== null || !enabled;
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
    if (cancelId && state.step === 'progress' && state.operation?.state !== 'rollingBack') {
      message.textContent = 'Cancellation requested. Waiting for the current step to finish…';
    }
    body.replaceChildren(); actions.replaceChildren();
    if (state.step === 'support') {
      if (view.canInstallSupport) {
        if (!emulatorIdle()) { const note = document.createElement('p'); note.textContent = 'Stop the emulator to install bridge support.'; body.append(note); }
        button(state.inspection?.support === 'needsUpdate' ? 'Update support' : 'Install support', () => void run(async () => {
          const inspection = await backend.install();
          if (inspection.support !== 'ready') throw { code: 'EINSTALL', message: 'Bridge support is not available yet. Check the system authorization prompt and try again.' };
          await inspectResult(inspection);
        }), emulatorIdle(), true);
      }
      button('Check again', () => void refresh(), true, !view.canInstallSupport);
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
          button('Repair', () => void plan({ kind: 'repair', resourceId: resource }), emulatorIdle() && state.inspection.readiness !== 'inUse' && !state.inspection.externalOverride);
          button('Remove setup', () => void plan({ kind: 'remove', resourceId: resource }), emulatorIdle() && state.inspection.readiness !== 'inUse');
        }
        if (state.inspection.platform === 'macos' && state.inspection.support === 'ready') {
          button('Uninstall bridge support', () => send({ type: 'reviewUninstall' }), emulatorIdle() && state.inspection.readiness !== 'inUse');
        }
      }
      button('Refresh', () => void refresh());
      const selected = state.inspection?.adapters.find(a => a.id === state.adapterId);
      button('Review setup', () => { if (selected) void plan({ kind: 'configure', adapterId: selected.id }); },
        !!selected?.eligible && selected.linkUp && !state.inspection?.externalOverride && emulatorIdle(), true);
    } else if (state.step === 'reviewUninstall') {
      button('Back', () => send({ type: 'back' }));
      button('Uninstall support', () => void run(async () => {
        if (!emulatorIdle() || state.step !== 'reviewUninstall') return;
        send({ type: 'uninstalling' });
        await backend.uninstall();
        send({ type: 'uninstalled' });
      }), emulatorIdle(), true);
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
        await apply(id);
      }), canApply(state, now(), emulatorIdle()), true);
    } else if (state.step === 'progress') {
      if (view.canCancel) {
        const id = state.operation?.id ?? state.plan?.id;
        if (id) button(state.operation?.confirmationDeadline ? 'Restore previous connection' : 'Cancel setup',
          () => cancel(id), true, false, true);
      }
      if (state.operation?.confirmationDeadline) {
        button('Keep connection', () => void run(async () => {
          if (canKeep(state, now())) receive(await backend.keep(state.operation!.id));
        }), canKeep(state, now()), true);
      }
    } else if (state.step === 'failed') {
      if (state.error?.code === 'EUPDATE' && state.inspection?.platform === 'macos') {
        button('Update bridge support', () => void run(async () => {
          const inspection = await backend.install();
          send({ type: 'back' });
          await inspectResult(inspection);
        }), emulatorIdle(), true);
      }
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
          await apply(id);
        }), canResume(state, emulatorIdle()), true);
      }
    }
    if (view.canClose) button(['ready', 'uninstalled'].includes(state.step) ? 'Done' : 'Close', () => dialog.close());
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
      if (b.textContent === 'Keep connection') b.disabled = busy || cancelId !== null || !canKeep(state, now());
      if (b.textContent === 'Continue setup') b.disabled = busy || cancelId !== null || !canResume(state, emulatorIdle());
      if (state.step === 'review' && (b.textContent === 'Apply setup' || b.textContent === 'Remove setup')) b.disabled = busy || cancelId !== null || !canApply(state, now(), emulatorIdle());
    }
  }
  dialog.addEventListener('cancel', event => { if (!bridgeView(state).canClose || busy || cancelId) event.preventDefault(); });
  render();
  dialog.showModal();
  heading.focus();
  const timer = window.setInterval(() => { tick(); void attemptCancel(); }, 1000);
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
