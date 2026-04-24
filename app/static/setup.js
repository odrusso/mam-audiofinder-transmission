document.addEventListener('DOMContentLoaded', () => {
  const form = document.getElementById('setupForm');
  const statusEl = document.getElementById('setupStatus');
  if (!form) return;

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const body = {
      mam_cookie: document.getElementById('mam_cookie')?.value.trim() || '',
      transmission_url: document.getElementById('transmission_url')?.value.trim() || '',
      transmission_user: document.getElementById('transmission_user')?.value.trim() || '',
      transmission_pass: document.getElementById('transmission_pass')?.value || '',
      transmission_label: document.getElementById('transmission_label')?.value.trim() || '',
      auto_import_enabled: document.getElementById('auto_import_enabled')?.checked || false,
    };

    statusEl.textContent = 'Saving…';

    try {
      const resp = await fetch('/api/setup', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!resp.ok) {
        let msg = `HTTP ${resp.status}`;
        try {
          const j = await resp.json();
          if (j?.detail) msg += ` — ${j.detail}`;
        } catch {}
        throw new Error(msg);
      }
      statusEl.textContent = 'Saved. You can now go back to the main app.';
    } catch (e) {
      console.error('setup save failed', e);
      statusEl.textContent = `Failed to save: ${e.message || e}`;
    }
  });
});
