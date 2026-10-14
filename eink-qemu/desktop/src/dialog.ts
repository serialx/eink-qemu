/* In-page prompt and confirm, styled with the page's tokens: the system
   WebViews do not all implement window.prompt. */
let open: HTMLDialogElement | null = null;
let count = 0;

export const dialogOpen = () => !!open;

function show(options: { title: string; message?: string; value?: string; ok: string; danger?: boolean; input: boolean }): Promise<string | null> {
  return new Promise((resolve) => {
    const dialog = document.createElement('dialog');
    dialog.className = 'ask';
    const id = `ask-${++count}`;
    dialog.setAttribute('aria-labelledby', `${id}-title`);
    const form = document.createElement('form');
    form.method = 'dialog';
    const title = document.createElement('strong');
    title.textContent = options.title;
    title.id = `${id}-title`;
    form.append(title);
    if (options.message) {
      const message = document.createElement('p');
      message.textContent = options.message;
      message.id = `${id}-message`;
      dialog.setAttribute('aria-describedby', message.id);
      form.append(message);
    }
    const input = document.createElement('input');
    if (options.input) {
      input.type = 'text';
      input.value = options.value ?? '';
      input.spellcheck = false;
      input.autocomplete = 'off';
      input.setAttribute('aria-labelledby', `${id}-title`);
      form.append(input);
    }
    const buttons = document.createElement('div');
    buttons.className = 'ask-buttons';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.textContent = 'Cancel';
    const ok = document.createElement('button');
    ok.type = 'submit';
    ok.className = options.danger ? 'danger' : 'primary';
    ok.textContent = options.ok;
    buttons.append(cancel, ok);
    form.append(buttons);
    dialog.append(form);
    document.body.append(dialog);
    let result: string | null = null;
    cancel.addEventListener('click', () => dialog.close());
    form.addEventListener('submit', () => { result = options.input ? input.value : 'ok'; });
    dialog.addEventListener('close', () => {
      dialog.remove();
      open = null;
      resolve(result);
    });
    open = dialog;
    dialog.showModal();
    if (options.input) {
      input.focus();
      // Select the name without its extension, like a file manager.
      const dot = input.value.lastIndexOf('.');
      input.setSelectionRange(0, dot > 0 ? dot : input.value.length);
    } else ok.focus();
  });
}

export const prompt = (title: string, value = '', ok = 'OK') => show({ title, value, ok, input: true });
export const confirm = async (title: string, message?: string, ok = 'OK', danger = false) =>
  (await show({ title, message, ok, danger, input: false })) === 'ok';
