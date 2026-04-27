// ---------- DOM lookups ----------
const form = document.getElementById('searchForm');
const q = document.getElementById('q');
const sortSel = document.getElementById('sort');
const perpageSel = document.getElementById('perpage');
const statusEl = document.getElementById('status');
const table = document.getElementById('results');
const tbody = table.querySelector('tbody');
const showHistoryBtn = document.getElementById('showHistoryBtn');
const historyCard = document.getElementById('historyCard');
const historyTable = document.getElementById('history');
const historyBody = historyTable.querySelector('tbody');
const historyColumnCount = historyTable.querySelector('thead tr').children.length;

let cachedTorrents = null;
let cachedTorrentsPromise = null;
let activeImportHistoryId = null;

const importRow = document.createElement('tr');
importRow.className = 'import-detail-row';
importRow.style.display = 'none';
const importCell = document.createElement('td');
importCell.colSpan = historyColumnCount;
importRow.appendChild(importCell);

function showHistoryCard() {
  historyCard.style.display = 'block';
}

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
  const sortType = sortSel?.value || 'default';
  const perpage = parseInt(perpageSel?.value || '25', 10);

  statusEl.textContent = 'Searching...';
  table.style.display = 'none';
  tbody.innerHTML = '';

  try {
    const data = await fetchJson('/search', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ tor: { text, sortType }, perpage })
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
              narrator: it.narrator_info || ''
            })
          });
          cachedTorrents = null;
          cachedTorrentsPromise = null;
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

function closeImportPanel() {
  activeImportHistoryId = null;
  importRow.style.display = 'none';
  importCell.innerHTML = '';
  if (importRow.parentNode) {
    importRow.parentNode.removeChild(importRow);
  }
}

async function getCompletedTorrents(forceReload = false) {
  if (forceReload) {
    cachedTorrents = null;
    cachedTorrentsPromise = null;
  }
  if (cachedTorrents) return cachedTorrents;
  if (!cachedTorrentsPromise) {
    cachedTorrentsPromise = fetchJson('/transmission/torrents').then((j) => {
      cachedTorrents = j.items || [];
      return cachedTorrents;
    }).catch((err) => {
      cachedTorrentsPromise = null;
      throw err;
    });
  }
  return cachedTorrentsPromise;
}

async function openImportPanel(historyItem, row) {
  if (activeImportHistoryId === historyItem.id) {
    closeImportPanel();
    return;
  }

  activeImportHistoryId = historyItem.id;
  importCell.innerHTML = `
    <div class="import-form import-panel">
      <div class="import-panel-row">
        <span>Import:</span>
        <span>/</span>
        <input type="text" class="imp-author" placeholder="Author" value="${escapeHtml(historyItem.author || '')}" style="min-width:220px;">
        <span>/</span>
        <input type="text" class="imp-title" placeholder="Title" value="${escapeHtml(historyItem.title || '')}" style="min-width:280px;">
        <span>/</span>
        <select class="imp-torrent" style="min-width:320px;">
          <option disabled selected>Loading torrents...</option>
        </select>
        <button class="imp-go">Copy to Library</button>
      </div>
      <div class="imp-status"></div>
    </div>
  `;
  row.after(importRow);
  importRow.style.display = '';

  const authorInput = importCell.querySelector('.imp-author');
  const titleInput = importCell.querySelector('.imp-title');
  const torrentSelect = importCell.querySelector('.imp-torrent');
  const goBtn = importCell.querySelector('.imp-go');
  const status = importCell.querySelector('.imp-status');

  try {
    const torrents = await getCompletedTorrents();
    if (activeImportHistoryId !== historyItem.id) return;

    torrentSelect.innerHTML = '';
    torrents.forEach((torrent) => {
      const option = document.createElement('option');
      option.value = torrent.hash;
      option.textContent = `${torrent.name} - ${torrent.single_file ? 'single-file' : (torrent.root || torrent.name)}`;
      torrentSelect.appendChild(option);
    });

    if (!torrentSelect.children.length) {
      const option = document.createElement('option');
      option.disabled = true;
      option.selected = true;
      option.textContent = 'No completed torrents with app label';
      torrentSelect.appendChild(option);
    }
  } catch (e) {
    console.error(e);
    if (activeImportHistoryId === historyItem.id) {
      torrentSelect.innerHTML = '<option disabled selected>Failed to load torrents</option>';
    }
  }

  goBtn.addEventListener('click', async (ev) => {
    ev.preventDefault();
    const author = authorInput.value.trim();
    const title = titleInput.value.trim();
    const hash = torrentSelect.value;

    if (!author || !title || !hash) {
      status.textContent = 'Please fill Author, Title, and select a torrent.';
      return;
    }

    goBtn.disabled = true;
    status.textContent = 'Importing...';

    try {
      const result = await fetchJson('/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          author,
          title,
          hash,
          history_id: historyItem.id
        })
      });

      cachedTorrents = null;
      cachedTorrentsPromise = null;
      status.textContent = `Done -> ${result.dest}`;
      goBtn.textContent = 'Imported';

      const statusTd = row.children[5];
      if (statusTd) statusTd.innerHTML = renderHistoryStatusCell({ torrent_status: 'imported' });
    } catch (e) {
      console.error(e);
      status.textContent = `Failed: ${e.message}`;
      const statusTd = row.children[5];
      if (statusTd) {
        statusTd.innerHTML = renderHistoryStatusCell({
          torrent_status: 'import_failed',
          status_detail: e.message || 'Import failed'
        });
      }
      goBtn.disabled = false;
    }
  });
}

async function loadHistory() {
  try {
    const j = await fetchJson('/history');
    closeImportPanel();
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

      const importBtn = document.createElement('button');
      importBtn.textContent = item.torrent_status === 'importing' ? 'Importing...' : 'Import';
      importBtn.disabled = item.torrent_status === 'importing';
      importBtn.addEventListener('click', async () => {
        await openImportPanel(item, tr);
      });

      const removeBtn = document.createElement('button');
      removeBtn.textContent = 'Remove';
      removeBtn.addEventListener('click', async () => {
        removeBtn.disabled = true;
        try {
          await fetchJson(`/history/${encodeURIComponent(item.id)}`, { method: 'DELETE' });
          if (activeImportHistoryId === item.id) closeImportPanel();
          tr.remove();
          if (!historyBody.children.length) renderEmptyHistory();
        } catch (e) {
          console.error('remove failed', e);
          removeBtn.disabled = false;
        }
      });

      tr.innerHTML = `
        <td>${escapeHtml(item.title || '')}</td>
        <td>${escapeHtml(item.author || '')}</td>
        <td>${escapeHtml(item.narrator || '')}</td>
        <td class="center">${linkURL ? `<a href="${linkURL}" target="_blank" rel="noopener noreferrer" title="Open on MAM">🔗</a>` : ''}</td>
        <td>${escapeHtml(when)}</td>
        <td>${renderHistoryStatusCell(item)}</td>
        <td></td>
        <td></td>
      `;

      applyDataLabels(historyTable, tr);
      tr.children[6].appendChild(importBtn);
      tr.children[7].appendChild(removeBtn);
      historyBody.appendChild(tr);
    });

    showHistoryCard();
  } catch (e) {
    console.error('history load failed', e);
  }
}
