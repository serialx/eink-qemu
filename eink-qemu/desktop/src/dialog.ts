/* In-page prompt and confirm, styled with the page's tokens: the system
   WebViews do not all implement window.prompt. */
let open: HTMLDialogElement | null = null;
let count = 0;

const managed = new Set<HTMLDialogElement>();
export const dialogOpen = () => !!open || managed.size > 0;

/** Registers custom native dialogs with the emulator's keyboard guard. */
export function registerDialog(dialog: HTMLDialogElement): () => void {
  managed.add(dialog);
  return () => managed.delete(dialog);
}

/** One option of a choice dialog; `note` is a second line under its label. */
export interface Choice { value: string; label: string; note?: string; disabled?: boolean }

function show(options: {
  title: string; message?: string; value?: string; ok: string; danger?: boolean; input: boolean;
  choices?: Choice[];
}): Promise<string | null> {
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
    const radios: HTMLInputElement[] = [];
    if (options.choices) {
      const list = document.createElement('div');
      list.className = 'ask-choices';
      list.setAttribute('role', 'radiogroup');
      list.setAttribute('aria-labelledby', `${id}-title`);
      for (const choice of options.choices) {
        const label = document.createElement('label');
        const radio = document.createElement('input');
        radio.type = 'radio';
        radio.name = id;
        radio.value = choice.value;
        radio.disabled = !!choice.disabled;
        radio.checked = choice.value === options.value;
        const text = document.createElement('span');
        const name = document.createElement('b');
        name.textContent = choice.label;
        text.append(name);
        if (choice.note) {
          const note = document.createElement('small');
          note.textContent = choice.note;
          text.append(note);
        }
        label.append(radio, text);
        list.append(label);
        radios.push(radio);
      }
      form.append(list);
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
    form.addEventListener('submit', () => {
      result = options.input ? input.value : options.choices ? radios.find((r) => r.checked)?.value ?? null : 'ok';
    });
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
    } else if (options.choices) (radios.find((r) => r.checked) ?? ok).focus();
    else ok.focus();
  });
}

export const prompt = (title: string, value = '', ok = 'OK') => show({ title, value, ok, input: true });
/** A choice among `choices`, starting at `value`; null when cancelled. */
export const choose = (title: string, message: string, choices: Choice[], value: string, ok = 'OK') =>
  show({ title, message, value, ok, input: false, choices });
export const confirm = async (title: string, message?: string, ok = 'OK', danger = false) =>
  (await show({ title, message, ok, danger, input: false })) === 'ok';
