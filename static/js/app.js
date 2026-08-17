const COPY_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const CHECK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
const EDIT_ICON = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>`;
const SUN_ICON = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>`;
const MOON_ICON = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>`;

let allDomains = [];
let currentFilter = 'new';
let pendingDomain = null;
let answerPendingDomain = null;
// /api/ai の応答。相手の一覧・選択・繋がらない理由が入る（取れなければ null）
let aiState = null;
// まとめて聞いている間は true（同時に2回走らせない・実行中に閉じさせない）
let bulkRunning = false;

// 1リクエストで聞く件数。**サーバ側の MAX_BULK_DOMAINS と同じ値にすること**
// （超えると 400 で断られる）。区切って何度も呼ぶので進捗が出せて、
// 途中で失敗してもそこまでのメモは残る
const BULK_CHUNK = 10;

// ---- テーマ（ライト/ダーク） ----
// 明示的に選んだらlocalStorageに残し、選んでいなければOSの設定に従う。
// 描画前の当て込みは index.html の <head> にある（body側だと一瞬もう一方の色が出る）

function effectiveTheme() {
  const explicit = document.documentElement.dataset.theme;
  if (explicit === 'dark' || explicit === 'light') return explicit;
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function renderTheme() {
  const dark = effectiveTheme() === 'dark';
  const btn = document.getElementById('theme-btn');
  // 出すのは**切り替わる先**の印。いまの状態を出すと「押したらどうなるか」が読めない
  btn.innerHTML = dark ? SUN_ICON : MOON_ICON;
  btn.title = dark ? 'ライトに切り替える' : 'ダークに切り替える';
  // アドレスバー・タスクスイッチャーの色も合わせる（合わないと帯だけ暗いままになる）
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.content = dark ? '#0d1117' : '#ffffff';
}

function toggleTheme() {
  const next = effectiveTheme() === 'dark' ? 'light' : 'dark';
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem('theme', next); } catch(e) { /* 保存できなくても切り替えは効く */ }
  renderTheme();
}

// OS の設定に従っている間は、OS 側の切り替えにボタンの印も追従させる
window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', renderTheme);

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

// いまのフィルターで画面に出ているドメイン。描画と「まとめて聞く」の対象が
// 同じ1か所から出るようにする（食い違うと、見えていない行にメモが付く）
function filteredDomains() {
  if (currentFilter === 'new') return allDomains.filter(d => !d.reviewed);
  if (currentFilter === 'reviewed') return allDomains.filter(d => d.reviewed);
  return allDomains;
}

function renderDomains() {
  const list = document.getElementById('domain-list');
  const filtered = filteredDomains();

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
      <!-- 未確認の行にバッジは出さない。**既読管理はしていないので「NEW」は嘘になる**
           （出していたのは「まだ確認済みにしていない」だけ）。それは左の赤い点と
           「確認済」バッジの有無で足りる -->
      ${d.reviewed ? '<span class="badge reviewed">確認済</span>' : ''}
      <!-- メモは確認済みかどうかに関わらず書ける（確認済みにしないと残せなかったのをやめた） -->
      <button class="action-btn edit-note-btn" data-domain="${escapeHtml(d.domain)}" data-note="${escapeHtml(d.note)}" onclick="openModal(this.dataset.domain, this.dataset.note)" title="${d.note ? 'メモを書き直す' : 'メモを書く'}">${EDIT_ICON}</button>
      ${!d.reviewed
        ? `<button class="action-btn review-btn" data-domain="${escapeHtml(d.domain)}" onclick="openModal(this.dataset.domain, this.dataset.note)">確認済みにする</button>`
        : `<button class="action-btn unreview-btn" data-domain="${escapeHtml(d.domain)}" onclick="unmarkReviewed(this.dataset.domain)">未確認に戻す</button>`
      }
    </div>
  `).join('');
}

function openModal(domain, existingNote = '') {
  pendingDomain = domain;
  document.getElementById('modal-domain').textContent = domain;
  document.getElementById('modal-note').value = existingNote;
  // 既に確認済みなら「確認済みにする」は出さない（押しても何も変わらないボタンを置かない）
  const item = allDomains.find(d => d.domain === domain);
  document.getElementById('modal-review-btn').hidden = !!(item && item.reviewed);
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


// ---- まとめてAIに聞く ----
// いま一覧に出ているドメインを BULK_CHUNK 件ずつAIに聞き、結果をメモとして残す。
// **確認済みにはしない** —— 調べただけの段階と、人が確認した段階は別

// 対象は「いま出ている行のうち、メモの無いもの」。**既にメモがある行は飛ばす**
// （人が書いたメモをAIの文章で上書きしないため。聞き直したいなら、その行のメモを
//   空にして保存すれば次の実行で対象に戻る）
function bulkTargets() {
  return filteredDomains().filter(d => !d.note || !d.note.trim()).map(d => d.domain);
}

function openBulkModal() {
  if (bulkRunning) { document.getElementById('bulk-modal').style.display = 'flex'; return; }
  const targets = bulkTargets();
  const shown = filteredDomains().length;
  const note = document.getElementById('bulk-note');
  note.className = 'bulk-note';
  note.textContent = targets.length === 0
    ? `いま出ている ${shown} 件はすべてメモがあります。聞き直したい行は、メモを空にして保存すると次の実行で対象に戻ります。`
    : `いま出ている ${shown} 件のうち、メモの無い ${targets.length} 件を ${aiName()} に聞き、`
      + `結果をメモとして残します（確認済みにはしません）。`
      + `${BULK_CHUNK} 件ずつ順に聞くので、途中で失敗してもそこまでは残ります。`;
  document.getElementById('bulk-log').innerHTML = '';
  document.getElementById('bulk-run-btn').disabled = targets.length === 0;
  document.getElementById('bulk-modal').style.display = 'flex';
}

function closeBulkModal() {
  // 実行中は閉じさせない（閉じると進捗が見えなくなるだけで、処理は止まらない）
  if (bulkRunning) return;
  document.getElementById('bulk-modal').style.display = 'none';
}

function onBulkOverlayClick(event) {
  if (event.target === document.getElementById('bulk-modal')) closeBulkModal();
}

function bulkLog(message, kind) {
  const log = document.getElementById('bulk-log');
  const line = document.createElement('div');
  line.className = `bulk-log-item ${kind || ''}`;
  line.textContent = message;
  log.appendChild(line);
  log.scrollTop = log.scrollHeight;
}

async function runBulkAsk() {
  if (bulkRunning) return;
  const targets = bulkTargets();
  if (targets.length === 0) return;

  bulkRunning = true;
  const runBtn = document.getElementById('bulk-run-btn');
  const closeBtn = document.getElementById('bulk-close-btn');
  runBtn.disabled = true;
  closeBtn.disabled = true;
  document.getElementById('bulk-log').innerHTML = '';

  let saved = 0;
  let failedChunks = 0;
  let author = '';

  for (let i = 0; i < targets.length; i += BULK_CHUNK) {
    const chunk = targets.slice(i, i + BULK_CHUNK);
    const note = document.getElementById('bulk-note');
    note.className = 'bulk-note';
    note.textContent = `${i} / ${targets.length} 件おわり — ${chunk.length} 件を聞いています…`;

    let result;
    try {
      const resp = await fetch('/api/ask-bulk', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({domains: chunk})
      });
      result = await resp.json();
    } catch(e) {
      result = {success: false, error: '通信に失敗しました'};
    }

    if (result.error === 'token_required') {
      // トークンが要るのはCLIブリッジ経由のとき。入れ直してもらう（残りは中断）
      bulkRunning = false;
      runBtn.disabled = false;
      closeBtn.disabled = false;
      document.getElementById('bulk-modal').style.display = 'none';
      openTokenModal();
      return;
    }

    if (result.success) {
      author = result.author || author;
      for (const entry of result.results) {
        const item = allDomains.find(d => d.domain === entry.domain);
        if (item) item.note = entry.note;
        saved++;
        bulkLog(`${entry.domain} — ${entry.note}`, 'done');
      }
      for (const domain of result.missing || []) {
        bulkLog(`${domain} — 答えが返りませんでした`, 'failed');
      }
    } else {
      failedChunks++;
      // **1回の失敗で止めない**（相手が一度崩れた応答を返しても、残りは聞ける）
      bulkLog(`${chunk.length} 件が失敗: ${result.error || '不明なエラー'}`, 'failed');
    }
    // サーバ側でメモは保存済み。押した人が結果を追えるよう、区切りごとに一覧へ反映する
    renderDomains();
  }

  bulkRunning = false;
  runBtn.disabled = bulkTargets().length === 0;
  closeBtn.disabled = false;

  const note = document.getElementById('bulk-note');
  note.className = failedChunks > 0 ? 'bulk-note error-text' : 'bulk-note';
  note.textContent = `${targets.length} 件のうち ${saved} 件にメモを付けました`
    + (author ? `（${author}）` : '')
    + (failedChunks > 0 ? ` — ${failedChunks} 回分は失敗しました。もう一度実行すると残りだけを聞きます。` : '');
  showToast(`${saved} 件にメモを付けました`, failedChunks > 0 ? 'error' : 'success');
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

function openTokenModal() {
  document.getElementById('token-input').value = '';
  document.getElementById('token-modal').style.display = 'flex';
  document.getElementById('token-input').focus();
}

function closeTokenModal() {
  document.getElementById('token-modal').style.display = 'none';
}

function onTokenOverlayClick(event) {
  if (event.target === document.getElementById('token-modal')) closeTokenModal();
}

async function submitClaudeToken() {
  const token = document.getElementById('token-input').value.trim();
  if (!token) return;
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
      // **自動では実行しない**（LLMの枠を使う操作を、保存の副作用で走らせない）。
      // 対象と件数を出すモーダルに戻して、押し直せるようにする
      openBulkModal();
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

// メモだけ保存する。確認済みかどうかは変えない
async function submitNote() {
  if (!pendingDomain) return;
  const domain = pendingDomain;
  const note = document.getElementById('modal-note').value.trim();
  closeModal();
  await saveNote(domain, note, `${domain} のメモを保存しました`);
}

// メモをサーバへ書き、手元の一覧にも反映する（一覧を読み直さずに済ませる）
async function saveNote(domain, note, successMessage) {
  try {
    const resp = await fetch('/api/note', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domain, note})
    });
    const result = await resp.json();
    if (result.success) {
      const item = allDomains.find(d => d.domain === domain);
      if (item) item.note = note;
      renderDomains();
      showToast(successMessage, 'success');
      return true;
    }
    showToast('メモの保存に失敗しました', 'error');
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
  return false;
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

function showToast(msg, type) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = `toast ${type} show`;
  setTimeout(() => t.classList.remove('show'), 3000);
}

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') { closeModal(); closeTokenModal(); closeAiModal(); closeBulkModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitNote();
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

renderTheme();
loadDomains();
loadAi();
