/* Console: the full log is kept in text; the filter only hides lines. */
const view = document.getElementById('console')!;
const filterInput = document.getElementById('console-filter') as HTMLInputElement;
const LIMIT = 1_000_000, KEEP = 800_000;
let text = '', filter = '';

const matches = (line: string) => !filter || line.toLowerCase().includes(filter);

function render() {
  view.textContent = filter ? text.split('\n').filter((line) => line && matches(line)).map((line) => line + '\n').join('') : text;
  view.scrollTop = view.scrollHeight;
}

export function appendLines(lines: string[]) {
  if (!lines.length) return;
  // Follow new output unless the reader has scrolled up.
  const follow = view.scrollHeight - view.scrollTop - view.clientHeight < 32;
  const chunk = lines.join('\n') + '\n';
  text += chunk;
  if (text.length > LIMIT) {
    text = text.slice(text.indexOf('\n', text.length - KEEP) + 1);
    render();
  } else {
    const shown = filter ? lines.filter(matches) : lines;
    if (shown.length) view.append(filter ? shown.join('\n') + '\n' : chunk);
  }
  if (follow) view.scrollTop = view.scrollHeight;
}

export function clearConsole() {
  text = '';
  render();
}

filterInput.addEventListener('input', () => {
  filter = filterInput.value.trim().toLowerCase();
  render();
});
document.getElementById('console-clear')!.addEventListener('click', clearConsole);
