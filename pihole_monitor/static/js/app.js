const COPY_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const CHECK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
const EDIT_ICON = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>`;
const CLAUDE_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><path d="M12 1.5l2.4 6.6 6.6 2.4-6.6 2.4L12 19.5l-2.4-6.6L3 10.5l6.6-2.4z"/></svg>`;

let allDomains = [];
let currentFilter = 'new';
let pendingDomain = null;
let claudePendingDomain = null;

function escapeHtml(str) {
  return str.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

async function loadDomains() {
  document.getElementById('domain-list').innerHTML = '<div class="loading">読み込み中...</div>';
  try {
    const resp = await fetch('/api/domains');
    if (!resp.ok) {
      showFetchError();
      return;
    }
    allDomains = await resp.json();
    updateStats();
    renderDomains();
  } catch(e) {
    showFetchError();
  }
}

function showFetchError() {
  allDomains = [];
  document.getElementById('stat-new').textContent = '-';
  document.getElementById('stat-reviewed').textContent = '-';
  document.getElementById('stat-total').textContent = '-';
  document.getElementById('domain-list').innerHTML = '<div class="empty"><div class="empty-icon">&#9888;</div>Pi-holeからの情報取得に失敗しました</div>';
}

function updateStats() {
  const newCount = allDomains.filter(d => !d.reviewed).length;
  const reviewedCount = allDomains.filter(d => d.reviewed).length;
  document.getElementById('stat-new').textContent = newCount;
  document.getElementById('stat-reviewed').textContent = reviewedCount;
  document.getElementById('stat-total').textContent = allDomains.length;
}

function setFilter(filter, event) {
  currentFilter = filter;
  document.querySelectorAll('.filter-btn').forEach(btn => btn.classList.remove('active'));
  event.target.classList.add('active');
  renderDomains();
}

function renderDomains() {
  const list = document.getElementById('domain-list');
  const filtered = currentFilter === 'new'
    ? allDomains.filter(d => !d.reviewed)
    : currentFilter === 'reviewed'
    ? allDomains.filter(d => d.reviewed)
    : allDomains;

  if (filtered.length === 0) {
    list.innerHTML = '<div class="empty"><div class="empty-icon">&#10003;</div>未確認のドメインはありません</div>';
    return;
  }

  list.innerHTML = filtered.map(d => `
    <div class="domain-item ${d.reviewed ? 'reviewed' : ''}">
      <div class="status-dot ${d.reviewed ? 'reviewed' : 'new'}"></div>
      <div class="domain-info">
        <div class="domain-name">${escapeHtml(d.domain)} <span class="domain-count">(${d.count})</span><button class="copy-btn" data-domain="${escapeHtml(d.domain)}" onclick="copyDomain(this)" title="コピー">${COPY_ICON}</button></div>
        ${d.note ? `<div class="domain-note">${escapeHtml(d.note)}</div>` : ''}
      </div>
      <span class="badge ${d.reviewed ? 'reviewed' : 'new'}">${d.reviewed ? '確認済' : 'NEW'}</span>
      <button class="action-btn ask-claude-btn" data-domain="${escapeHtml(d.domain)}" onclick="askClaude(this.dataset.domain)" title="Claudeに聞く">${CLAUDE_ICON} Claude</button>
      ${!d.reviewed
        ? `<button class="action-btn review-btn" data-domain="${escapeHtml(d.domain)}" onclick="openModal(this.dataset.domain)">確認済みにする</button>`
        : `<button class="action-btn edit-note-btn" data-domain="${escapeHtml(d.domain)}" data-note="${escapeHtml(d.note)}" onclick="openModal(this.dataset.domain, this.dataset.note)" title="メモを書き直す">${EDIT_ICON}</button>
           <button class="action-btn unreview-btn" data-domain="${escapeHtml(d.domain)}" onclick="unmarkReviewed(this.dataset.domain)">未確認に戻す</button>`
      }
    </div>
  `).join('');
}

function openModal(domain, existingNote = '') {
  pendingDomain = domain;
  document.getElementById('modal-domain').textContent = domain;
  document.getElementById('modal-note').value = existingNote;
  document.getElementById('modal').style.display = 'flex';
  document.getElementById('modal-note').focus();
}

function closeModal() {
  document.getElementById('modal').style.display = 'none';
  pendingDomain = null;
}

function onOverlayClick(event) {
  if (event.target === document.getElementById('modal')) closeModal();
}

function openClaudeModal(domain) {
  claudePendingDomain = domain;
  document.getElementById('claude-modal-domain').textContent = domain;
  const body = document.getElementById('claude-modal-body');
  body.textContent = 'Claudeに問い合わせ中...';
  body.className = 'claude-modal-body loading-text';
  const item = allDomains.find(d => d.domain === domain);
  document.getElementById('claude-modal-note').value = item ? (item.note || '') : '';
  document.getElementById('claude-modal').style.display = 'flex';
}

function closeClaudeModal() {
  document.getElementById('claude-modal').style.display = 'none';
  claudePendingDomain = null;
}

function onClaudeOverlayClick(event) {
  if (event.target === document.getElementById('claude-modal')) closeClaudeModal();
}

async function askClaude(domain) {
  openClaudeModal(domain);
  const body = document.getElementById('claude-modal-body');
  try {
    const resp = await fetch('/api/ask-claude', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain})
    });
    const result = await resp.json();
    if (result.success) {
      body.textContent = result.answer;
      body.className = 'claude-modal-body';
    } else if (result.error === 'token_required') {
      closeClaudeModal();
      openTokenModal(domain);
    } else {
      body.textContent = `Claudeへの問い合わせに失敗しました（${result.error || '不明なエラー'}）`;
      body.className = 'claude-modal-body error-text';
    }
  } catch(e) {
    body.textContent = 'Claudeへの問い合わせに失敗しました';
    body.className = 'claude-modal-body error-text';
  }
}

let tokenPendingDomain = null;

function openTokenModal(domain) {
  tokenPendingDomain = domain;
  document.getElementById('token-input').value = '';
  document.getElementById('token-modal').style.display = 'flex';
  document.getElementById('token-input').focus();
}

function closeTokenModal() {
  document.getElementById('token-modal').style.display = 'none';
  tokenPendingDomain = null;
}

function onTokenOverlayClick(event) {
  if (event.target === document.getElementById('token-modal')) closeTokenModal();
}

async function submitClaudeToken() {
  const token = document.getElementById('token-input').value.trim();
  if (!token) return;
  const domain = tokenPendingDomain;
  try {
    const resp = await fetch('/api/claude-token', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({token})
    });
    const result = await resp.json();
    if (result.success) {
      closeTokenModal();
      showToast('トークンを保存しました', 'success');
      if (domain) askClaude(domain);
    } else {
      showToast('トークンの保存に失敗しました', 'error');
    }
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
}

function copyDomain(btn) {
  navigator.clipboard.writeText(btn.dataset.domain).then(() => {
    btn.innerHTML = CHECK_ICON;
    btn.classList.add('copied');
    setTimeout(() => { btn.innerHTML = COPY_ICON; btn.classList.remove('copied'); }, 1500);
  });
}

async function unmarkReviewed(domain) {
  try {
    const resp = await fetch('/api/review', {
      method: 'DELETE',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain})
    });
    const result = await resp.json();
    if (result.success) {
      const item = allDomains.find(d => d.domain === domain);
      if (item) { item.reviewed = false; item.note = ''; }
      updateStats();
      renderDomains();
      showToast(`${domain} を未確認に戻しました`, 'success');
    } else {
      showToast('失敗しました', 'error');
    }
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
}

async function submitReview() {
  if (!pendingDomain) return;
  const domain = pendingDomain;
  const note = document.getElementById('modal-note').value.trim();
  closeModal();

  try {
    const resp = await fetch('/api/review', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain, note})
    });
    const result = await resp.json();
    if (result.success) {
      const item = allDomains.find(d => d.domain === domain);
      if (item) { item.reviewed = true; item.note = note; }
      updateStats();
      renderDomains();
      showToast(`${domain} を確認済みにしました`, 'success');
    } else {
      showToast('失敗しました', 'error');
    }
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
}

async function submitReviewFromClaudeModal() {
  if (!claudePendingDomain) return;
  const domain = claudePendingDomain;
  const note = document.getElementById('claude-modal-note').value.trim();

  try {
    const resp = await fetch('/api/review', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain, note})
    });
    const result = await resp.json();
    if (result.success) {
      const item = allDomains.find(d => d.domain === domain);
      if (item) { item.reviewed = true; item.note = note; }
      updateStats();
      renderDomains();
      closeClaudeModal();
      showToast(`${domain} を確認済みにしました`, 'success');
    } else {
      showToast('失敗しました', 'error');
    }
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
}

function showToast(msg, type) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = `toast ${type} show`;
  setTimeout(() => t.classList.remove('show'), 3000);
}

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') { closeModal(); closeClaudeModal(); closeTokenModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitReview();
    if (document.getElementById('claude-modal').style.display !== 'none') submitReviewFromClaudeModal();
    if (document.getElementById('token-modal').style.display !== 'none') submitClaudeToken();
  }
});

loadDomains();
