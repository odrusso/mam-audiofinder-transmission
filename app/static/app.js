// ---------- DOM lookups ----------
const form = document.getElementById('searchForm');
const q = document.getElementById('q');
const mediaTypeInputs = Array.from(document.querySelectorAll('input[name="mediaType"]'));
const perpageSel = document.getElementById('perpage');
const statusEl = document.getElementById('status');
const table = document.getElementById('results');
const tbody = table.querySelector('tbody');
const showHistoryBtn = document.getElementById('showHistoryBtn');
const historyCard = document.getElementById('historyCard');
const historyTable = document.getElementById('history');
const historyBody = historyTable.querySelector('tbody');
const historyColumnCount = historyTable.querySelector('thead tr').children.length;

function showHistoryCard() {
  historyCard.style.display = 'block';
}

function getSelectedMediaType() {
  const selected = document.querySelector('input[name="mediaType"]:checked')?.value;
  return selected === 'ebook' ? 'ebook' : 'audiobook';
}

function normalizeMediaType(value) {
  return value === 'ebook' ? 'ebook' : 'audiobook';
}

function mediaTypeLabel(value) {
  return normalizeMediaType(value) === 'ebook' ? 'Ebook' : 'Audiobook';
}

function renderMediaTypeBadge(value) {
  return `<span class="type-badge">${escapeHtml(mediaTypeLabel(value))}</span>`;
}

function updateSearchPlaceholder() {
  if (!q) return;
  q.placeholder = getSelectedMediaType() === 'ebook'
    ? 'Search title/author'
    : 'Search title/author/narrator';
}

mediaTypeInputs.forEach((input) => input.addEventListener('change', updateSearchPlaceholder));
updateSearchPlaceholder();

// Focus the search box on devices where it will not pop open a touch keyboard.
if (q && window.matchMedia && window.matchMedia('(hover: hover) and (pointer: fine)').matches) q.focus();

// ---------- Show History (even without searching) ----------
if (showHistoryBtn) {
  showHistoryBtn.addEventListener('click', async () => {
    showHistoryCard();
    await loadHistory();
    historyCard.scrollIntoView({ behavior: 'smooth', block: 'start' });
  });
}

// ---------- Submit handler (Enter or button) ----------
if (form) {
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    await runSearch();
  });
}

// ---------- Search flow ----------
async function runSearch() {
  const text = (q?.value || '').trim();
  const mediaType = getSelectedMediaType();
  const perpage = parseInt(perpageSel?.value || '25', 10);

  statusEl.textContent = 'Searching...';
  table.style.display = 'none';
  tbody.innerHTML = '';

  try {
    const data = await fetchJson('/search', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ media_type: mediaType, tor: { text }, perpage })
    });

    const rows = data.results || [];
    if (!rows.length) {
      statusEl.textContent = 'No results.';
      return;
    }

    rows.forEach((it) => {
      const tr = document.createElement('tr');
      const detailsURL = it.id ? `https://www.myanonamouse.net/t/${encodeURIComponent(it.id)}` : '';
      const addBtn = document.createElement('button');
      addBtn.textContent = 'Add';
      addBtn.disabled = !(it.dl || it.id);
      addBtn.addEventListener('click', async () => {
        addBtn.disabled = true;
        addBtn.textContent = 'Adding...';
        try {
          await fetchJson('/add', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
              id: String(it.id ?? ''),
              title: it.title || '',
              dl: it.dl || '',
              author: it.author_info || '',
              narrator: it.narrator_info || '',
              media_type: it.media_type || mediaType
            })
          });
          addBtn.textContent = 'Added';
          await loadHistory();
        } catch (e) {
          console.error(e);
          addBtn.textContent = 'Error';
          addBtn.disabled = false;
        }
      });

      tr.innerHTML = `
        <td>${renderResultTitleCell(it)}</td>
        <td>${escapeHtml(it.author_info || '')}</td>
        <td>${escapeHtml(it.narrator_info || '')}</td>
        <td>${escapeHtml(it.format || '')}</td>
        <td class="right">${formatSize(it.size)}</td>
        <td class="right">${escapeHtml(`${it.seeders ?? '-'} / ${it.leechers ?? '-'}`)}</td>
        <td>${escapeHtml(it.added || '')}</td>
        <td class="center">
          ${detailsURL ? `<a href="${detailsURL}" target="_blank" rel="noopener noreferrer" title="Open on MAM">🔗</a>` : ''}
        </td>
        <td></td>
      `;

      applyDataLabels(table, tr);
      tr.lastElementChild.appendChild(addBtn);
      tbody.appendChild(tr);
    });

    table.style.display = '';
    statusEl.textContent = `${rows.length} results shown`;
    await loadHistory();
  } catch (e) {
    console.error(e);
    statusEl.textContent = 'Search failed.';
  }
}

// ---------- Helpers ----------
function escapeHtml(s) {
  return (s || '').toString()
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

function truncateText(text, maxLen = 140) {
  const value = (text || '').trim();
  if (!value || value.length <= maxLen) return value;
  return `${value.slice(0, maxLen - 1)}…`;
}

function renderResultTitleCell(item) {
  const badges = [];
  if (item?.is_freeleech) {
    badges.push('<span class="result-badge result-badge-free">Freeleech</span>');
  }
  if (item?.is_vip) {
    badges.push('<span class="result-badge result-badge-vip">VIP</span>');
  }

  const badgesHtml = badges.length
    ? `<div class="result-flags">${badges.join('')}</div>`
    : '';

  return `
    <div class="result-title-cell">
      <div class="result-title-main">${escapeHtml(item?.title || '')}</div>
      ${badgesHtml}
    </div>
  `;
}

function renderHistoryStatusCell(item) {
  const status = item?.torrent_status || '';
  const detail = item?.status_detail || '';
  const classes = [];
  if (status === 'import_failed') classes.push('history-status-failed');
  if (status === 'importing') classes.push('history-status-active');

  const statusHtml = classes.length
    ? `<span class="${classes.join(' ')}">${escapeHtml(status)}</span>`
    : escapeHtml(status);
  const detailHtml = detail
    ? `<div class="history-status-detail">${escapeHtml(truncateText(detail))}</div>`
    : '';

  return `${statusHtml}${detailHtml}`;
}

function formatSize(sz) {
  if (sz == null || sz === '') return '';
  const n = Number(sz);
  if (!Number.isFinite(n)) return String(sz);
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let x = n;
  while (x >= 1024 && i < units.length - 1) {
    x /= 1024;
    i += 1;
  }
  return `${x.toFixed(1)} ${units[i]}`;
}

async function fetchJson(url, options) {
  const resp = await fetch(url, options);
  if (resp.ok) return resp.json();

  let msg = `HTTP ${resp.status}`;
  try {
    const j = await resp.json();
    if (j?.detail) msg += ` - ${j.detail}`;
  } catch {}
  throw new Error(msg);
}

function renderEmptyHistory() {
  const tr = document.createElement('tr');
  tr.className = 'empty';
  tr.innerHTML = `<td colspan="${historyColumnCount}" class="center muted">No items in history yet.</td>`;
  historyBody.appendChild(tr);
}

function applyDataLabels(sourceTable, row) {
  const labels = Array.from(sourceTable.querySelectorAll('thead th'))
    .map((th) => th.textContent.trim());

  Array.from(row.children).forEach((cell, index) => {
    if (labels[index]) cell.dataset.label = labels[index];
  });
}

async function loadHistory() {
  try {
    const j = await fetchJson('/history');
    historyBody.innerHTML = '';

    const items = j.items || [];
    if (!items.length) {
      renderEmptyHistory();
      showHistoryCard();
      return;
    }

    items.forEach((item) => {
      const tr = document.createElement('tr');
      const when = item.added_at ? new Date(item.added_at.replace(' ', 'T') + 'Z').toLocaleString() : '';
      const linkURL = item.mam_id ? `https://www.myanonamouse.net/t/${encodeURIComponent(item.mam_id)}` : '';
      const mediaType = normalizeMediaType(item.media_type);

      tr.innerHTML = `
        <td>${renderMediaTypeBadge(mediaType)}</td>
        <td>${escapeHtml(item.title || '')}</td>
        <td>${escapeHtml(item.author || '')}</td>
        <td>${escapeHtml(item.narrator || '')}</td>
        <td class="center">${linkURL ? `<a href="${linkURL}" target="_blank" rel="noopener noreferrer" title="Open on MAM">🔗</a>` : ''}</td>
        <td>${escapeHtml(when)}</td>
        <td>${renderHistoryStatusCell(item)}</td>
      `;

      applyDataLabels(historyTable, tr);
      historyBody.appendChild(tr);
    });

    showHistoryCard();
  } catch (e) {
    console.error('history load failed', e);
  }
}
