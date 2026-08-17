const COPY_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const CHECK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
const EDIT_ICON = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>`;
const AI_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><path d="M12 1.5l2.4 6.6 6.6 2.4-6.6 2.4L12 19.5l-2.4-6.6L3 10.5l6.6-2.4z"/></svg>`;

let allDomains = [];
let currentFilter = 'new';
let pendingDomain = null;
let answerPendingDomain = null;
// /api/ai の応答。相手の一覧・選択・繋がらない理由が入る（取れなければ null）
let aiState = null;

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
      <button class="action-btn ask-ai-btn" data-domain="${escapeHtml(d.domain)}" onclick="askAi(this.dataset.domain)" title="このドメインについてAIに聞く">${AI_ICON} AIに聞く</button>
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

function openAnswerModal(domain) {
  answerPendingDomain = domain;
  document.getElementById('answer-modal-domain').textContent = domain;
  const body = document.getElementById('answer-modal-body');
  body.textContent = `${aiName()}に問い合わせ中...`;
  body.className = 'answer-body loading-text';
  document.getElementById('answer-modal-author').textContent = '';
  const item = allDomains.find(d => d.domain === domain);
  document.getElementById('answer-modal-note').value = item ? (item.note || '') : '';
  document.getElementById('answer-modal').style.display = 'flex';
}

function closeAnswerModal() {
  document.getElementById('answer-modal').style.display = 'none';
  answerPendingDomain = null;
}

function onAnswerOverlayClick(event) {
  if (event.target === document.getElementById('answer-modal')) closeAnswerModal();
}

async function askAi(domain) {
  openAnswerModal(domain);
  const body = document.getElementById('answer-modal-body');
  const author = document.getElementById('answer-modal-author');
  try {
    const resp = await fetch('/api/ask', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain})
    });
    const result = await resp.json();
    if (result.success) {
      body.textContent = result.answer;
      body.className = 'answer-body';
      // 応答が名乗った相手を出す（「相手の既定に任せる」で頼んだときは
      // これだけが何が書いたかの手がかり）
      author.textContent = result.author ? `— ${result.author}` : '';
    } else if (result.error === 'token_required') {
      // トークンが要るのはCLIブリッジ経由のときだけ
      closeAnswerModal();
      openTokenModal(domain);
    } else {
      body.textContent = `AIへの問い合わせに失敗しました（${result.error || '不明なエラー'}）`;
      body.className = 'answer-body error-text';
    }
  } catch(e) {
    body.textContent = 'AIへの問い合わせに失敗しました';
    body.className = 'answer-body error-text';
  }
}

// ---- 聞く相手の選択（Chiezo） ----
// 相手はChiezo（LAN内の知識サーバー）に登録してあるものから選ぶ。鍵はあちらが
// 持っているので、こちらは相手を選ぶだけでよい。Chiezoが未設定なら選択肢は
// CLIブリッジ（Claude Code）1つだけになる

async function loadAi() {
  try {
    const resp = await fetch('/api/ai');
    aiState = resp.ok ? await resp.json() : null;
  } catch(e) {
    aiState = null;
  }
  renderAiButton();
}

// いま聞く相手の名前。取れていないときも「AI」と出す（ボタンを空にしない）
function aiName() {
  return (aiState && aiState.current) ? aiState.current : 'AI';
}

function renderAiButton() {
  document.getElementById('ai-btn').textContent = `AI: ${aiName()}`;
}

function openAiModal() {
  renderAiList();
  document.getElementById('ai-modal').style.display = 'flex';
  // 開いたときに一覧を取り直す。Chiezoを後から起動した場合に、
  // 画面を読み直さなくても相手が出てくるようにする
  loadAi().then(() => renderAiList());
}

function closeAiModal() {
  document.getElementById('ai-modal').style.display = 'none';
}

function onAiOverlayClick(event) {
  if (event.target === document.getElementById('ai-modal')) closeAiModal();
}

function renderAiList() {
  const note = document.getElementById('ai-note');
  const list = document.getElementById('ai-list');

  if (!aiState) {
    note.className = 'ai-note error-text';
    note.textContent = '相手の一覧を取得できませんでした。ページを読み直してください。';
    list.innerHTML = '';
    return;
  }

  // 一覧が空の理由を言い分ける（未設定なのか、届かないのか）
  if (!aiState.chiezo_url) {
    note.className = 'ai-note';
    note.innerHTML = 'Chiezo の URL が未設定です。環境変数 <code>CHIEZO_BASE_URL</code> に '
      + '<code>http://192.168.1.x:7010</code> のような URL を入れて起動すると、'
      + 'ここで相手を選べます（<code>/v1</code> は付けない）。';
  } else if (aiState.error) {
    note.className = 'ai-note error-text';
    note.textContent = aiState.error;
  } else if (aiState.backends.length === 0) {
    note.className = 'ai-note';
    note.textContent = `Chiezo（${aiState.chiezo_url}）に話せる相手がいません。Chiezo 側で「答える」層を有効にしてください。`;
  } else {
    note.className = 'ai-note';
    note.textContent = '選んだ相手が、以後すべてのドメインの説明を書きます（再起動なしで切り替わります）。';
  }

  const selected = aiState.selection ? aiState.selection.backend : '';
  // 先頭は従来の経路。**消さずに残す** —— Chiezoが落ちている日にも聞けるようにするため
  const rows = [`
    <div class="ai-row">
      <label class="ai-row-main">
        <input type="radio" name="ai-backend" value="" ${selected ? '' : 'checked'}>
        <span class="ai-row-name">${escapeHtml(aiState.bridge_label)}</span>
      </label>
      <div class="ai-row-opts"><span class="ai-row-hint">トークンの登録が要ります</span></div>
    </div>
  `];

  for (const backend of aiState.backends) {
    const choice = selected === backend.id ? aiState.selection : null;
    const model = choice && choice.model ? choice.model : '';
    const effort = choice && choice.effort ? choice.effort : '';
    rows.push(`
      <div class="ai-row">
        <label class="ai-row-main">
          <input type="radio" name="ai-backend" value="${escapeHtml(backend.id)}" ${choice ? 'checked' : ''}>
          <span class="ai-row-name">${escapeHtml(backend.label)}</span>
        </label>
        <div class="ai-row-opts">
          <select class="ai-select" data-role="model" data-backend="${escapeHtml(backend.id)}" aria-label="モデル">
            ${backend.model_required ? '' : `<option value="" ${model ? '' : 'selected'}>モデル: 相手の既定</option>`}
            ${backend.models.map(m => `<option value="${escapeHtml(m)}" ${m === model ? 'selected' : ''}>${escapeHtml(m)}</option>`).join('')}
          </select>
          ${backend.efforts.length === 0 ? '' : `
          <select class="ai-select" data-role="effort" data-backend="${escapeHtml(backend.id)}" aria-label="考える量">
            <option value="" ${effort ? '' : 'selected'}>考える量: 相手の既定</option>
            ${backend.efforts.map(v => `<option value="${escapeHtml(v)}" ${v === effort ? 'selected' : ''}>${escapeHtml(v)}</option>`).join('')}
          </select>`}
        </div>
      </div>
    `);
  }

  list.innerHTML = rows.join('');
}

async function saveAiSelection() {
  const checked = document.querySelector('input[name="ai-backend"]:checked');
  if (!checked) return;
  const backend = checked.value;
  const value = role => {
    const el = document.querySelector(`select[data-role="${role}"][data-backend="${backend}"]`);
    return el ? el.value : '';
  };

  const note = document.getElementById('ai-note');
  try {
    const resp = await fetch('/api/ai', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({backend: backend || null, model: value('model'), effort: value('effort')})
    });
    const result = await resp.json();
    if (result.success) {
      closeAiModal();
      // 保存した値そのものを画面へ反映する（Chiezoへ聞き直さない）
      await loadAi();
      showToast(`${result.current} に聞くようにしました`, 'success');
    } else {
      // 失敗の理由はモーダルに残す（閉じてしまうと読めない）
      note.className = 'ai-note error-text';
      note.textContent = result.error || '保存に失敗しました';
    }
  } catch(e) {
    note.className = 'ai-note error-text';
    note.textContent = '保存に失敗しました';
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
      if (domain) askAi(domain);
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

async function submitReviewFromAnswerModal() {
  if (!answerPendingDomain) return;
  const domain = answerPendingDomain;
  const note = document.getElementById('answer-modal-note').value.trim();

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
      closeAnswerModal();
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
  if (e.key === 'Escape') { closeModal(); closeAnswerModal(); closeTokenModal(); closeAiModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitReview();
    if (document.getElementById('answer-modal').style.display !== 'none') submitReviewFromAnswerModal();
    if (document.getElementById('token-modal').style.display !== 'none') submitClaudeToken();
    if (document.getElementById('ai-modal').style.display !== 'none') saveAiSelection();
  }
});

// モデル・考える量をいじったら、その行を選んだものとして扱う（選び直しの手数を減らす）。
// **一覧はinnerHTMLで差し替えるので、リスナーは入れ物に1回だけ付ける**
document.getElementById('ai-list').addEventListener('change', e => {
  if (e.target.tagName !== 'SELECT') return;
  const radio = document.querySelector(`input[name="ai-backend"][value="${e.target.dataset.backend}"]`);
  if (radio) radio.checked = true;
});

loadDomains();
loadAi();
