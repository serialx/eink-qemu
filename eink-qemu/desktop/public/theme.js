// Before the first paint: the saved theme, and the platform for the macOS
// title bar inset. A file rather than an inline script, so the CSP needs no
// 'unsafe-inline' for scripts.
try { const theme = localStorage.getItem('eink-qemu-theme'); if (theme) document.documentElement.dataset.theme = theme; } catch {}
if ('__TAURI_INTERNALS__' in window && navigator.userAgent.includes('Mac')) document.documentElement.dataset.platform = 'macos';
