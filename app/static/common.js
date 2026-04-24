(async () => {
  const healthEl = document.getElementById('health');
  if (!healthEl) return;

  try {
    const r = await fetch('/health');
    const j = await r.json();
    healthEl.textContent = j.ok ? `OK${j.version ? ` (v${j.version})` : ''}` : 'Not OK';
  } catch {
    healthEl.textContent = 'Error';
  }
})();
