const COPY_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const CHECK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
const AI_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><path d="M12 1.5l2.4 6.6 6.6 2.4-6.6 2.4L12 19.5l-2.4-6.6L3 10.5l6.6-2.4z"/></svg>`;
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
// 行のボタンで聞いている最中のドメイン。**再描画をまたいで残す** ——
// 行のDOMは renderDomains() で作り直されるので、ボタン側に状態を持たせると消える
const askingDomains = new Set();
// 相手を選ぶモーダルを開くときに出したい一言（トークン未登録で開いたときなど）
let aiModalMessage = null;
// チェックした行。**再描画をまたいで残す**（行のDOMは作り直される）
const selectedDomains = new Set();

// 「まとめてAIに聞く」1回の上限の既定。**未確認は常に全件聞き直す**ので、
// 枠と時間を使いすぎないための歯止めが要る（画面で変えられ、localStorageに残る）
const BULK_LIMIT_DEFAULT = 20;

// 1リクエストで聞く件数。**サーバ側の MAX_DOMAINS_PER_ASK と同じ値にすること**
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
    renderSelection(filtered);
    list.innerHTML = '<div class="empty"><div class="empty-icon">&#10003;</div>未確認のドメインはありません</div>';
    return;
  }

  // 選択のバー（件数・ボタンの有効/無効）も一覧と一緒に描き直す
  renderSelection(filtered);

  list.innerHTML = filtered.map(d => `
    <div class="domain-item ${d.reviewed ? 'reviewed' : ''}">
      <input type="checkbox" class="row-check" data-domain="${escapeHtml(d.domain)}"
             onchange="toggleSelect(this)" ${selectedDomains.has(d.domain) ? 'checked' : ''}
             aria-label="この行を選ぶ">
      <div class="status-dot ${d.reviewed ? 'reviewed' : 'new'}"></div>
      <div class="domain-info">
        <div class="domain-name">${escapeHtml(d.domain)} <span class="domain-count">(${d.count})</span><button class="copy-btn" data-domain="${escapeHtml(d.domain)}" onclick="copyDomain(this)" title="コピー">${COPY_ICON}</button></div>
        ${d.note ? `<div class="domain-note">${escapeHtml(d.note)}</div>` : ''}
      </div>
      <!-- 未確認の行にバッジは出さない。**既読管理はしていないので「NEW」は嘘になる**
           （出していたのは「まだ確認済みにしていない」だけ）。それは左の赤い点と
           「確認済」バッジの有無で足りる -->
      ${d.reviewed ? '<span class="badge reviewed">確認済</span>' : ''}
      <!-- 1件だけ聞く。**答えはそのままメモになる**（回答を見せるモーダルは無い）。
           聞いている間は押せないようにする —— 状態は askingDomains に持つ（この行のDOMは
           再描画で作り直されるため） -->
      ${askingDomains.has(d.domain)
        ? `<button class="action-btn ask-ai-btn" disabled>${AI_ICON} 聞いています…</button>`
        : `<button class="action-btn ask-ai-btn" data-domain="${escapeHtml(d.domain)}" onclick="askOne(this.dataset.domain)" title="${d.note ? 'AIに聞いてメモを置き換える' : 'AIに聞いてメモにする'}">${AI_ICON} AIに聞く</button>`
      }
      <!-- メモは確認済みかどうかに関わらず書ける（確認済みにしないと残せなかったのをやめた） -->
      <button class="action-btn edit-note-btn" data-domain="${escapeHtml(d.domain)}" data-note="${escapeHtml(d.note)}" onclick="openModal(this.dataset.domain, this.dataset.note)" title="${d.note ? 'メモを書き直す' : 'メモを書く'}">${EDIT_ICON}</button>
      ${!d.reviewed
        ? `<button class="action-btn review-btn" data-domain="${escapeHtml(d.domain)}" onclick="openModal(this.dataset.domain, this.dataset.note)">確認済みにする</button>`
        : `<button class="action-btn unreview-btn" data-domain="${escapeHtml(d.domain)}" onclick="unmarkReviewed(this.dataset.domain)">未確認に戻す</button>`
      }
    </div>
  `).join('');
}

// ---- チェックした行の一括操作 ----

function toggleSelect(checkbox) {
  const domain = checkbox.dataset.domain;
  if (checkbox.checked) selectedDomains.add(domain); else selectedDomains.delete(domain);
  renderSelection(filteredDomains());
}

function toggleSelectAll(checkbox) {
  // **対象は「表示中」だけ**。フィルターで見えていない行まで選ぶと、
  // 何を確認済みにしたのか押した人に分からない
  for (const d of filteredDomains()) {
    if (checkbox.checked) selectedDomains.add(d.domain); else selectedDomains.delete(d.domain);
  }
  renderDomains();
}

function clearSelection() {
  selectedDomains.clear();
  renderDomains();
}

// バーの表示を一覧に合わせる。**一覧から消えた行の選択は落とす** ——
// 見えないものを確認済みにしないため
function renderSelection(filtered) {
  const visible = new Set(filtered.map(d => d.domain));
  for (const domain of [...selectedDomains]) {
    if (!visible.has(domain)) selectedDomains.delete(domain);
  }

  const count = selectedDomains.size;
  document.getElementById('select-count').textContent = `${count} 件選択`;
  document.getElementById('select-review-btn').disabled = count === 0;
  document.getElementById('select-clear-btn').disabled = count === 0;
  const all = document.getElementById('select-all');
  all.checked = filtered.length > 0 && count === filtered.length;
}

// チェックした行をまとめて確認済みにする。**メモは送らない** ——
// サーバ側が「渡されなければ触らない」ので、AIに聞いた結果が消えない
async function reviewSelected() {
  const domains = [...selectedDomains];
  if (domains.length === 0) return;
  const btn = document.getElementById('select-review-btn');
  btn.disabled = true;

  try {
    const resp = await fetch('/api/review', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domains})
    });
    const result = await resp.json();
    if (result.success) {
      for (const domain of domains) {
        const item = allDomains.find(d => d.domain === domain);
        if (item) item.reviewed = true;
      }
      selectedDomains.clear();
      updateStats();
      renderDomains();
      showToast(`${result.count || domains.length} 件を確認済みにしました`, 'success');
      return;
    }
    showToast('確認済みにできませんでした', 'error');
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
  renderSelection(filteredDomains());
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


// ---- 1件だけAIに聞く ----
// 押したらそのままメモになる（回答を見せるモーダルは持たない）。保存はサーバ側で
// 済んでいるので、ここでやるのは手元の一覧への反映だけ。
// **まとめて聞くのと同じ口**（/api/ask に1件だけ渡す）—— 指示文も保存の仕方も1か所に保つ

async function askOne(domain) {
  if (askingDomains.has(domain)) return;
  askingDomains.add(domain);
  renderDomains();

  let result;
  try {
    const resp = await fetch('/api/ask', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domains: [domain]})
    });
    result = await resp.json();
  } catch(e) {
    result = {success: false, error: '通信に失敗しました'};
  }
  askingDomains.delete(domain);

  if (result.error === 'token_required') {
    // トークンが要るのはCLIブリッジ経由のとき。設定はそこにあるので相手を選ぶモーダルを開く
    renderDomains();
    openAiModal('トークンが未登録です。CLIブリッジの行に貼り付けて保存するか、Chiezo の相手を選んでください。');
    return;
  }

  const entry = result.success && result.results && result.results[0];
  if (entry) {
    const item = allDomains.find(d => d.domain === entry.domain);
    if (item) item.note = entry.note;
    renderDomains();
    // 誰が書いたか（複数いる）と、答えられなかった相手を一緒に出す ——
    // **1人落ちても残りは使う**ので、成功と失敗が同時に起きうる
    const failed = (result.failures || []).length;
    showToast(
      `${domain} のメモを付けました（${(result.authors || []).join(' / ')}）`
        + (failed ? ` — ${failed} 人は答えられませんでした` : ''),
      failed ? 'error' : 'success');
    return;
  }

  renderDomains();
  showToast(`聞けませんでした（${result.error || '答えが返りませんでした'}）`, 'error');
}

// ---- まとめてAIに聞く ----
// いま一覧に出ているドメインを BULK_CHUNK 件ずつAIに聞き、結果をメモとして残す。
// **確認済みにはしない** —— 調べただけの段階と、人が確認した段階は別

// 対象は**未確認の全件**。メモがあっても聞き直す —— 確認済みにしていない行は
// 「まだ判断していない」ので、最新の見立てで上書きしてよい。**確認済みは触らない**
// （人が確認したメモをAIの文章で上書きしないため）。
// **フィルターに関係なく未確認を見る**（表示を切り替えただけで対象が変わると分かりにくい）
function bulkTargets() {
  return allDomains.filter(d => !d.reviewed).map(d => d.domain);
}

// 1回の実行で聞く上限。**覚えておく**（毎回入れ直させない）
function bulkLimit() {
  const stored = parseInt(localStorage.getItem('bulkLimit'), 10);
  return Number.isFinite(stored) && stored > 0 ? stored : BULK_LIMIT_DEFAULT;
}

function saveBulkLimit(input) {
  const value = parseInt(input.value, 10);
  if (Number.isFinite(value) && value > 0) {
    try { localStorage.setItem('bulkLimit', String(value)); } catch(e) { /* 保存できなくても効く */ }
  }
  input.value = bulkLimit();
  openBulkModal();
}

function openBulkModal() {
  if (bulkRunning) { document.getElementById('bulk-modal').style.display = 'flex'; return; }
  document.getElementById('bulk-limit').value = bulkLimit();

  const targets = bulkTargets();
  const limited = targets.slice(0, bulkLimit());
  const note = document.getElementById('bulk-note');
  note.className = 'bulk-note';
  note.textContent = targets.length === 0
    ? '未確認のドメインがありません。'
    : `未確認 ${targets.length} 件のうち ${limited.length} 件を ${aiName()} に聞き、`
      + `結果をメモとして残します（メモがあっても聞き直します。確認済みの行は触りません）。`
      + `${BULK_CHUNK} 件ずつ順に聞くので、途中で失敗してもそこまでは残ります。`
      + (targets.length > limited.length
          ? ` 残り ${targets.length - limited.length} 件は上限のため今回は聞きません。`
          : '');
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
  const targets = bulkTargets().slice(0, bulkLimit());
  if (targets.length === 0) return;

  bulkRunning = true;
  const runBtn = document.getElementById('bulk-run-btn');
  const closeBtn = document.getElementById('bulk-close-btn');
  runBtn.disabled = true;
  closeBtn.disabled = true;
  document.getElementById('bulk-log').innerHTML = '';

  let saved = 0;
  let failedChunks = 0;
  // 実際に書いた相手（複数いるので集合で持つ）
  const authors = new Set();

  for (let i = 0; i < targets.length; i += BULK_CHUNK) {
    const chunk = targets.slice(i, i + BULK_CHUNK);
    const note = document.getElementById('bulk-note');
    note.className = 'bulk-note';
    note.textContent = `${i} / ${targets.length} 件おわり — ${chunk.length} 件を聞いています…`;

    let result;
    try {
      const resp = await fetch('/api/ask', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({domains: chunk})
      });
      result = await resp.json();
    } catch(e) {
      result = {success: false, error: '通信に失敗しました'};
    }

    if (result.error === 'token_required') {
      // トークンが要るのはCLIブリッジ経由のとき。設定は相手を選ぶモーダルにあるので、
      // そちらへ送る（残りは中断。入れ直したら押し直せる）
      bulkRunning = false;
      runBtn.disabled = false;
      closeBtn.disabled = false;
      document.getElementById('bulk-modal').style.display = 'none';
      openAiModal('トークンが未登録です。CLIブリッジの行に貼り付けて保存するか、Chiezo の相手を選んでください。');
      return;
    }

    if (result.success) {
      for (const name of result.authors || []) authors.add(name);
      for (const message of result.failures || []) bulkLog(message, 'failed');
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
    + (authors.size ? `（${[...authors].join(' / ')}）` : '')
    + (failedChunks > 0 ? ` — ${failedChunks} 回分は失敗しました。もう一度実行すると同じ対象を聞き直します。` : '');
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

// いま聞く相手の名前（複数）。取れていないときも空にしない
function aiNames() {
  return (aiState && aiState.current && aiState.current.length) ? aiState.current : ['AI'];
}

// 案内文に出す表記。全員を並べる
function aiName() {
  return aiNames().join(' / ');
}

function renderAiButton() {
  const names = aiNames();
  // **ボタンには先頭 + 残りの人数**。全員並べるとツールバーが名前で埋まる
  document.getElementById('ai-btn').textContent =
    `AI: ${names[0]}${names.length > 1 ? ` +${names.length - 1}` : ''}`;
}

function openAiModal(message) {
  aiModalMessage = message || null;
  renderAiList();
  document.getElementById('ai-modal').style.display = 'flex';
  // 開いたときに一覧を取り直す。Chiezoを後から起動した場合に、
  // 画面を読み直さなくても相手が出てくるようにする
  loadAi().then(() => renderAiList());
}

function closeAiModal() {
  document.getElementById('ai-modal').style.display = 'none';
  aiModalMessage = null;
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

  // 開いた理由（トークン未登録など）があれば、それを最優先で出す
  if (aiModalMessage) {
    note.className = 'ai-note error-text';
    note.textContent = aiModalMessage;
  } else if (!aiState.chiezo_url) {
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
    note.textContent = '選んだ相手ぜんぶに同じ内容を聞き、答えを「誰が書いたか」付きで'
      + '1つのメモに並べます（再起動なしで切り替わります）。';
  }

  // 選択は複数。**保存済みの選択から、相手ごとのモデル・考える量を引く**
  const chosen = new Map((aiState.selections || []).map(sel => [sel.backend, sel]));
  // 先頭は従来の経路。**消さずに残す** —— Chiezoが落ちている日にも聞けるようにするため。
  // **これも選択肢の1つ**なので、Chiezoの相手と一緒に選べる（両方に聞いて読み比べられる）
  const rows = [`
    <div class="ai-row">
      <label class="ai-row-main">
        <input type="checkbox" name="ai-backend" value="${escapeHtml(aiState.bridge_backend)}"
               ${chosen.has(aiState.bridge_backend) ? 'checked' : ''}>
        <span class="ai-row-name">${escapeHtml(aiState.bridge_label)}</span>
      </label>
      <!-- トークンの設定はこの行の中。**値は出さない**（登録済みかどうかだけ隣に出す） -->
      <div class="ai-row-opts">
        <span class="ai-row-hint">トークン: ${aiState.token_saved ? '登録済み' : '未登録'}</span>
        <input type="password" class="ai-token" id="token-input" autocomplete="off"
               placeholder="${aiState.token_saved ? '入れ替えるとき貼り付け' : 'トークンを貼り付け'}">
        <button type="button" class="action-btn review-btn" onclick="saveToken()">保存</button>
      </div>
    </div>
  `];

  for (const backend of aiState.backends) {
    const choice = chosen.get(backend.id);
    const model = choice && choice.model ? choice.model : '';
    const effort = choice && choice.effort ? choice.effort : '';
    rows.push(`
      <div class="ai-row">
        <label class="ai-row-main">
          <input type="checkbox" name="ai-backend" value="${escapeHtml(backend.id)}" ${choice ? 'checked' : ''}>
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
  // **チェックした全員を送る。** 0人なら「未選択」= CLIブリッジに戻る
  const selections = [...document.querySelectorAll('input[name="ai-backend"]:checked')].map(box => {
    const backend = box.value;
    const value = role => {
      const el = document.querySelector(`select[data-role="${role}"][data-backend="${backend}"]`);
      return el ? el.value : '';
    };
    return {backend, model: value('model'), effort: value('effort')};
  });

  const note = document.getElementById('ai-note');
  try {
    const resp = await fetch('/api/ai', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({selections})
    });
    const result = await resp.json();
    if (result.success) {
      closeAiModal();
      // 保存した値そのものを画面へ反映する（Chiezoへ聞き直さない）
      await loadAi();
      showToast(`${(result.current || []).join(' / ')} に聞くようにしました`, 'success');
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

// CLIブリッジのトークンを保存する（相手を選ぶモーダルの中から呼ばれる）
async function saveToken() {
  const field = document.getElementById('token-input');
  const note = document.getElementById('ai-note');
  const token = field.value.trim();
  if (!token) {
    note.className = 'ai-note error-text';
    note.textContent = 'トークンを貼り付けてから「保存」を押してください。';
    return;
  }

  try {
    const resp = await fetch('/api/claude-token', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({token})
    });
    const result = await resp.json();
    if (result.success) {
      // 「登録済み」の表示を取り直す。**保存の副作用でAIを呼ばない**
      // （枠を使う操作は、押した人が改めて押して始める）
      aiModalMessage = null;
      await loadAi();
      renderAiList();
      showToast('トークンを保存しました', 'success');
    } else {
      note.className = 'ai-note error-text';
      note.textContent = result.error || 'トークンの保存に失敗しました';
    }
  } catch(e) {
    note.className = 'ai-note error-text';
    note.textContent = 'トークンの保存に失敗しました';
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
      body: JSON.stringify({domains: [domain]})
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
      body: JSON.stringify({domains: [domain], note})
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
      body: JSON.stringify({domains: [domain], note})
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
  if (e.key === 'Escape') { closeModal(); closeAiModal(); closeBulkModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitNote();
    if (document.getElementById('ai-modal').style.display !== 'none') saveAiSelection();
  }
});

// モデル・考える量をいじったら、その行を選んだものとして扱う（選び直しの手数を減らす）。
// **一覧はinnerHTMLで差し替えるので、リスナーは入れ物に1回だけ付ける**
document.getElementById('ai-list').addEventListener('change', e => {
  if (e.target.tagName !== 'SELECT') return;
  const box = document.querySelector(`input[name="ai-backend"][value="${e.target.dataset.backend}"]`);
  if (box) box.checked = true;
});

renderTheme();
loadDomains();
loadAi();
