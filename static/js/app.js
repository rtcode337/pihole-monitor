const COPY_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const CHECK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`;
const AI_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><path d="M12 1.5l2.4 6.6 6.6 2.4-6.6 2.4L12 19.5l-2.4-6.6L3 10.5l6.6-2.4z"/></svg>`;
const EDIT_ICON = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>`;
const SUN_ICON = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>`;
const MOON_ICON = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>`;

let allDomains = [];
let currentFilter = 'new';
// 'blocked' = Pi-holeが止めたもの / 'watch' = 素通りしているものの中の怪しい候補。
// 同じ一覧の変数に入れるので、フィルター・選択・まとめて聞く・詳細モーダルは
// どちらのモードでもそのまま動く（行が持つ項目が増えるだけ）
let currentMode = 'blocked';
// いま出している一覧の付帯情報（`/api/domains`・`/api/watch` の応答の、items 以外の全部）。
// 前置きの文と Pi-hole のクエリログへのリンクをここから組む —— どちらのモードでも
// 「一覧そのもの」と「どこまで見えているか」を1つの応答で返してもらう
let listMeta = null;
let pendingDomain = null;
let answerPendingDomain = null;
// /api/ai の応答。相手の一覧・選択・繋がらない理由が入る（取れなければ null）
let aiState = null;
// まとめて聞いている間は true（同時に2回走らせない・実行中に閉じさせない）
let bulkRunning = false;
// 行のボタンで聞いている最中のドメイン。再描画をまたいで残す ——
// 行のDOMは renderDomains() で作り直されるので、ボタン側に状態を持たせると消える
const askingDomains = new Set();
// 相手を選ぶモーダルを開くときに出したい一言（トークン未登録で開いたときなど）
// 詳細を開いているドメイン。ドメイン名だけを持つ（オブジェクトを持つと、
// AIに聞いた後の allDomains の更新が詳細に映らない）
let detailDomain = null;
// 詳細画面で書きかけの追加質問。変数に持つ —— 詳細の中身は renderDetail() が
// innerHTML で作り直すので、textarea に入れたままだと聞いている間に消える
let followupDraft = '';
// チェックした行。再描画をまたいで残す（行のDOMは作り直される）
const selectedDomains = new Set();

// 「まとめてAIに聞く」1回の上限の既定。枠と時間を使いすぎないための歯止め
// （画面で変えられ、localStorageに残る）
const BULK_LIMIT_DEFAULT = 100;

// 1リクエストで聞く件数。サーバ側の MAX_DOMAINS_PER_ASK と同じ値にすること
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
  // 出すのは切り替わる先の印。いまの状態を出すと「押したらどうなるか」が読めない
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

// ISO8601 を「8/19 00:12」の形にする。秒とタイムゾーンは出さない ——
// 見出しの添え物なので、いつ調べたかが分かれば足りる
function shortTime(iso) {
  const d = new Date(iso);
  if (isNaN(d)) return '';
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

// unix秒 → 「8/19 00:12」。書式は shortTime に集約する（同じ画面で表記を割らない）
function shortTimeAt(unixSecs) {
  return shortTime(new Date(unixSecs * 1000).toISOString());
}

function escapeHtml(str) {
  return str.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

async function loadDomains() {
  document.getElementById('domain-list').innerHTML = '<div class="loading">読み込み中...</div>';
  try {
    const resp = await fetch(currentMode === 'watch' ? '/api/watch' : '/api/domains');
    if (!resp.ok) {
      showFetchError();
      return;
    }
    const body = await resp.json();
    listMeta = body;
    allDomains = body.items || [];
    renderContext();
    updateStats();
    renderDomains();
  } catch(e) {
    showFetchError();
  }
}

// unix秒 → datetime-local が受け取る形（ローカル時刻の "YYYY-MM-DDTHH:MM"）。
// UTCのISO文字列をそのまま入れない —— 入力欄はローカル時刻として読むので9時間ずれる
function toLocalInput(unixSecs) {
  const d = new Date(unixSecs * 1000);
  const p = n => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

// 基準日時を保存する。`useNow` なら入力欄を見ずに現在時刻にする
// （ネットワークの設定を変えた直後はこれが一番よく使う操作）
async function saveBaseline(useNow) {
  let at;
  if (useNow) {
    at = Math.floor(Date.now() / 1000);
  } else {
    const raw = document.getElementById('baseline-at').value;
    if (!raw) { showToast('日時を入れてください', 'error'); return; }
    at = Math.floor(new Date(raw).getTime() / 1000);
    if (!Number.isFinite(at)) { showToast('日時を読めませんでした', 'error'); return; }
  }
  await postBaseline(at, `${toLocalInput(at)} 以降を見るようにしました`);
}

async function clearBaseline() {
  await postBaseline(null, '基準日時を解除しました（直近24時間に戻ります）');
}

async function postBaseline(at, message) {
  try {
    const resp = await fetch('/api/watch/baseline', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({at})
    });
    const result = await resp.json();
    if (result.success) {
      showToast(message, 'success');
      // 保存したら読み直す。 候補も前置きもこの日時から作り直される
      loadDomains();
      return;
    }
    showToast(result.error || '保存できませんでした', 'error');
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
}

// 一覧の前置き。「どこまで見えているか」を両方のモードで必ず出す ——
// 監視は、材料が貯まっていない時期の「0件」を平穏だと読み違えないため。
// ブロック済みは、件数（Pi-hole の集計）とアクセス元・期間（手元に貯めたクエリ）で
// 出どころが違うため —— 断らずに並べると、保持期間より前のことまで書いてあるように読める
function renderContext() {
  const box = document.getElementById('list-context');
  const bar = document.getElementById('watch-baseline');
  if (!listMeta) { box.hidden = true; bar.hidden = true; return; }
  const m = listMeta;

  if (currentMode !== 'watch') {
    // 基準日時は監視だけの設定（ブロック済みの一覧には窓が無い）
    bar.hidden = true;
    box.innerHTML = blockedContextHtml(m);
    box.hidden = false;
    return;
  }

  // 基準日時の入力欄を、いまの設定に合わせる（未設定なら空）
  bar.hidden = false;
  document.getElementById('baseline-at').value = m.baseline ? toLocalInput(m.baseline) : '';

  const parts = [];
  if (!m.ready) {
    parts.push('<strong class="watch-warn">⚠️ 過去の取り込みがまだ終わっていません。'
      + 'いまは「はじめて見た」が当てになりません</strong>（過去を知らないので、すべてが初出に見えます）。');
  }
  if (m.baseline_clamped) {
    parts.push(`<strong class="watch-warn">⚠️ 基準日時が古すぎるので、過去 ${m.backfill_days} 日に丸めました</strong>`
      + `（それより前は「はじめて見た」を判定できません）。`);
  }
  parts.push(m.baseline
    ? `<strong>${escapeHtml(shortTimeAt(m.since))} 以降</strong>`
      + `（${m.window_hours} 時間ぶん）を、過去 ${m.backfill_days} 日ぶんの記録`
      + `（${(m.total_domains||0).toLocaleString()} ドメイン）と突き合わせています。`
      + `件数と種別は手元に貯まっている ${Math.floor(m.data_hours)} 時間ぶんが対象です。`
    : `直近 ${m.window_hours} 時間を、過去 ${m.backfill_days} 日ぶんの記録（${(m.total_domains||0).toLocaleString()} ドメイン）と`
      + `突き合わせています。件数と種別は手元に貯まっている ${Math.floor(m.data_hours)} 時間ぶんが対象です。`);
  if (m.qtypes && m.qtypes.length) {
    const shown = m.qtypes.map(([t, n]) => `${t} ${n.toLocaleString()}`).join(' / ');
    parts.push(`<span class="watch-qtypes">この間に出たクエリ種別: ${escapeHtml(shown)}</span>`);
  }
  box.innerHTML = parts.join('<br>') + methodsHtml(m);
  box.hidden = false;
}

// ブロック済みの一覧の前置き。1つの行に出どころの違う数字が並ぶので、そこを断る
function blockedContextHtml(m) {
  const base = '<strong>件数</strong>は Pi-hole の集計（設定の「取り込む件数」ぶん）です。';
  if (!m.data_since) {
    return base + '<br><strong>アクセス元と期間</strong>は貯めたクエリから出しますが、'
      + 'まだ1件も貯まっていません（取り込みが済むと出ます）。';
  }
  return base + `<br><strong>アクセス元と期間</strong>は、手元に貯まっている記録`
    + `（${escapeHtml(shortTimeAt(m.data_since))} 以降）の中で、`
    + `Pi-hole が止めたクエリだけを数えたものです。`
    + `それより前のブロックは、件数に入っていても期間には出ません。`;
}

// 「どうやって候補を選んでいるか」。畳んでおく（普段は前置きの数行だけ読めばよく、
// 「なぜこれが挙がったのか腑に落ちない」ときに開く）。
// 文はサーバがしきい値から組み立てたものをそのまま出す —— ここに散文で書き写すと、
// 定数を変えたときに説明だけが古くなる
function methodsHtml(m) {
  if (!m.methods || !m.methods.length) return '';
  const rows = m.methods.map(x => `
    <li class="watch-method">
      <div class="watch-method-head">
        <span class="reason reason-${escapeHtml(x.kind)}">${escapeHtml(x.label)}</span>
        <span class="watch-method-catches">${escapeHtml(x.catches)}</span>
      </div>
      <div class="watch-method-how">${escapeHtml(x.how)}</div>
      <div class="watch-method-caveat">※ ${escapeHtml(x.caveat)}</div>
    </li>`).join('');

  return `<details class="watch-methods">
    <summary>どうやって候補を選んでいるか（${m.methods.length} つの手）</summary>
    <p class="watch-method-lead">
      素通りしている通信は1日に千件を超えるので、全部は並べません。下の手のどれかに
      当たったものだけを出しています。<strong>どれも「いつもと違う」の言い換え</strong>で、
      過去の記録と突き合わせて判定しています。判定はこのアプリの中のルールで行っていて、
      AIには渡していません（AIに聞くのは、絞り込んだ後の「これは何か」だけ）。
      Pi-holeがブロックしたかどうかでは絞っていないので、ブロック済みのドメインが
      混ざることもあります（<strong>その行には「Pi-holeが止めている」の印が付きます</strong>。
      止められているのに端末が鳴らし続けていること自体が見たい情報なので、落としていません）。<strong>理由の札を押すと、Pi-holeのクエリログで
      その通信だけを絞り込んで開きます。</strong>
    </p>
    <ul class="watch-method-list">${rows}</ul>
  </details>`;
}

function showFetchError() {
  allDomains = [];
  // 前置きも消す。 取れなかったのに「どこまで見えているか」だけ前の応答のまま残ると、
  // 一覧が空なのか読めなかったのかが画面から区別できない
  listMeta = null;
  renderContext();
  document.getElementById('stat-new').textContent = '-';
  document.getElementById('stat-reviewed').textContent = '-';
  document.getElementById('stat-total').textContent = '-';
  document.getElementById('domain-list').innerHTML = '<div class="empty"><div class="empty-icon">&#9888;</div>Pi-holeからの情報取得に失敗しました</div>';
}

function updateStats() {
  const newCount = allDomains.filter(d => !d.reviewed).length;
  // 見たいのは「あとどれだけ確認が残っているか」なので、確認済みの数はそのまま出す
  const reviewedCount = allDomains.filter(d => d.reviewed).length;
  document.getElementById('stat-new').textContent = newCount;
  document.getElementById('stat-reviewed').textContent = reviewedCount;
  document.getElementById('stat-total').textContent = allDomains.length;
  // 3つ目の数字は意味が変わる。 ブロック済みでは「止めた総数」だが、
  // 監視では「挙がった候補の数」——同じラベルのままだと嘘になる
  document.getElementById('stat-total-label').textContent =
    currentMode === 'watch' ? '怪しい候補' : 'ブロック総数';
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

// 候補が挙がった理由。必ず出す —— 「なぜこれが並んでいるのか」が読めない一覧は、
// 誤検知か本物かを人が判断できず、結局まるごと無視されることになる。
// ブロック済みの行には reasons が無いので、そのときは何も出さない
// 確認済みのバッジ。未確認の行には出さない —— 既読管理はしていないので
// 「NEW」は嘘になる。未確認であることは左の赤い点で足りる
function reviewedBadge(d) {
  return d.reviewed ? '<span class="badge reviewed">確認済</span>' : '';
}

// Pi-hole のクエリログを絞り込んで開く URL（理由の札と、行の「Pi-holeで見る」）。
// パラメータ名は Pi-hole のものをサーバがそのまま返している（domain / client_ip /
// type / reply）ので、ここは値をエンコードして並べるだけでよい —— 手ごとに違う絞り込み方
// （種別なら type、NXDOMAIN なら reply）を画面側に散らかさない。
// 時間の範囲は判定に使った窓とそろえる（画面が持つ現在時刻は使わない）。
// Pi-hole の管理画面の URL が分からなければ空を返し、呼び出し側がリンクをやめる
function piholeQueryUrl(filter) {
  if (!listMeta || !listMeta.pihole_url || !filter) return '';
  const params = Object.entries(filter).map(([k, v]) => `${k}=${encodeURIComponent(v)}`);
  if (!params.length) return '';
  // 時間の範囲は監視のときだけ付く（応答に窓が入っているのはあちらだけ）。
  // ブロック済みの一覧には窓が無いので、Pi-hole 側の既定の範囲で開く
  if (listMeta.since) params.push(`from=${listMeta.since}`);
  if (listMeta.until) params.push(`until=${listMeta.until}`);
  return `${listMeta.pihole_url}/admin/queries.lp?${params.join('&')}`;
}

// AIに渡す「候補に挙げた理由」。画面に出している文をそのまま渡す ——
// 人が読んでいる根拠とAIが見ている根拠が食い違うと、答えが当たっているのか判断できない
function reasonText(d) {
  if (!d || !d.reasons || !d.reasons.length) return '';
  const parts = [d.reasons.map(r => r.detail).join(' / ')];
  if (d.clients && d.clients.length) parts.push(`この間に引いた端末: ${d.clients.join(', ')}`);
  // 止められていることもそのまま渡す —— 画面に「Pi-holeが止めている」と出ているのに
  // AIが素通りしている前提で答えると、読み比べたときにどちらが正しいのか分からない
  const blocked = blockedMark(d);
  if (blocked) parts.push(blocked);
  return parts.join('。');
}

function reasonsHtml(d) {
  if (!d.reasons || !d.reasons.length) return '';
  const items = d.reasons
    .map(r => {
      const cls = `reason reason-${escapeHtml(r.kind)}`;
      const url = piholeQueryUrl(r.filter);
      // 押したら元の通信が見られるようにする。 「規則正しく鳴っている」と言われても、
      // 本当にそうなっているかは Pi-hole のクエリログを見るのが一番早い。
      // リンクは一覧の行の中にあるが、詳細モーダルは開かない（委譲した click が a を除いている）
      if (!url) return `<span class="${cls}">${escapeHtml(r.detail)}</span>`;
      return `<a class="${cls} reason-link" href="${escapeHtml(url)}" target="_blank" rel="noopener"`
        + ` title="Pi-holeのクエリログで、この通信だけを絞り込んで開く">${escapeHtml(r.detail)}</a>`;
    })
    .join('');
  return `<div class="reasons">${items}</div>`;
}

// Pi-hole が止めているかどうかの印。監視の一覧はブロックの有無で絞っていないので、
// 止められているドメインも挙がる —— 落とさずに印を付ける（止められているのに端末が
// 鳴らし続けていること自体が見たい情報で、一覧から消すとそれが見えなくなる）。
// 一部だけ止まっている場合は数を添える（端末ごとの設定・CNAME経由で分かれる）。
// ブロック済みの一覧では出さない（全部が止まったものなので、印にならない）
function blockedMark(d) {
  if (currentMode !== 'watch' || !d.blocked_count) return '';
  return d.blocked_count >= d.count
    ? 'Pi-holeが止めている'
    : `一部をPi-holeが止めている（${d.count} 回中 ${d.blocked_count} 回）`;
}

// 「まだ続いている」とみなす、最後の通信からの間隔（秒）。
// 一覧を開いた時点で鳴りやんだばかりのものまで拾いたいので、分単位では短すぎる
const ONGOING_SECS = 3600;

// 最後の通信がついさっきか（＝いまも続いている通信か）。
// 判定はサーバの時刻でする（応答の `now`）—— 画面の時計で測ると、端末の時計が
// ずれているだけで常に続いて見えたり、逆に一度も続かなくなる
// （記録の時刻は Pi-hole 側の時計で付いている）
function isOngoing(d) {
  const now = listMeta && listMeta.now;
  return !!(now && d.active_to && now - d.active_to <= ONGOING_SECS);
}

// 通信が起きていた期間。同じ分に収まるなら1つだけ出す
// （「8/19 21:03 〜 8/19 21:03」は幅を取るだけで何も足さない）。
// いまも続いているものは終わりを太字にする —— 一覧で見分けたいのは
// 「もう止まった通信」と「いま鳴っている通信」で、日時を読み比べないと
// 分からないのでは何十行も並んだときに拾えない
function periodHtml(d) {
  if (!d.active_from || !d.active_to) return '';
  const from = escapeHtml(shortTimeAt(d.active_from));
  const to = escapeHtml(shortTimeAt(d.active_to));
  const ongoing = isOngoing(d);
  const end = ongoing ? `<strong class="period-now">${to}</strong>` : to;
  const title = ongoing
    ? `この範囲で通信が起きていた期間（直近1時間以内にも出ている＝いまも続いている）`
    : 'この範囲で通信が起きていた期間';
  const body = from === to ? end : `${from} 〜 ${end}`;
  return `<span class="period" title="${title}">${body}</span>`;
}

// 観測した事実（期間・引いた端末・Pi-hole のクエリログへの入口）。
// 両方のモードで同じ形にする —— 「誰が・いつからいつまで」はどちらの一覧でも
// 同じ問いに答えるものなので、片方だけ別の見せ方にすると読み替えが要る。
// どの端末が引いたかは判断材料そのもの（PCが引くのと家電が引くのでは意味が違う）。
// 範囲は前置きが断っている（監視は見ている窓、ブロック済みは手元に貯まっている記録）
function activityHtml(d) {
  const parts = [];
  const blocked = blockedMark(d);
  if (blocked) {
    parts.push(`<span class="blocked-mark" title="この一覧はブロックの有無で絞っていないので、`
      + `Pi-holeが止めているドメインも挙がります">${escapeHtml(blocked)}</span>`);
  }
  const period = periodHtml(d);
  if (period) parts.push(period);
  if (d.clients && d.clients.length) {
    parts.push(`<span class="reason-clients">${escapeHtml(d.clients.join(', '))}</span>`);
  }
  // ドメインで絞り込んだクエリログ。理由の札（監視）と違って条件はドメインだけ ——
  // 「本当にそうなっているか」は元の通信を見るのが一番早い
  const url = piholeQueryUrl({domain: d.domain});
  if (url) {
    parts.push(`<a class="pihole-link" href="${escapeHtml(url)}" target="_blank" rel="noopener"`
      + ` title="Pi-holeのクエリログを、このドメインで絞り込んで開く">Pi-holeで見る</a>`);
  }
  return parts.length ? `<div class="activity">${parts.join('')}</div>` : '';
}

function renderDomains() {
  const list = document.getElementById('domain-list');
  const filtered = filteredDomains();

  if (filtered.length === 0) {
    renderSelection(filtered);
    list.innerHTML = currentMode === 'watch'
      ? '<div class="empty"><div class="empty-icon">&#10003;</div>いまの窓では、挙がった候補はありません</div>'
      : '<div class="empty"><div class="empty-icon">&#10003;</div>未確認のドメインはありません</div>';
    return;
  }

  // 選択のバー（件数・ボタンの有効/無効）も一覧と一緒に描き直す
  renderSelection(filtered);

  // 操作は .domain-actions でひとまとめにする。 狭い画面ではこの塊ごと次の行へ落として
  // ドメインとメモに幅を明け渡す（style.css の @media）—— 個々のボタンを行に直接並べていた
  // ままだと、幅の足りない画面で domain-info が0幅まで潰れ、1文字ずつ縦に折り返して読めなくなる
  list.innerHTML = filtered.map(d => `
    <div class="domain-item ${d.reviewed ? 'reviewed' : ''}" data-domain="${escapeHtml(d.domain)}">
      <input type="checkbox" class="row-check" data-domain="${escapeHtml(d.domain)}"
             onchange="toggleSelect(this)" ${selectedDomains.has(d.domain) ? 'checked' : ''}
             aria-label="この行を選ぶ">
      <div class="status-dot ${d.reviewed ? 'reviewed' : 'new'}"></div>
      <div class="domain-info">
        <div class="domain-name">${escapeHtml(d.domain)} <span class="domain-count">(${d.count})</span><button class="copy-btn" data-domain="${escapeHtml(d.domain)}" onclick="copyDomain(this)" title="コピー">${COPY_ICON}</button></div>
        ${reasonsHtml(d)}
        ${activityHtml(d)}
        ${d.note ? `<div class="domain-note">${escapeHtml(d.note)}</div>` : ''}
      </div>
      <div class="domain-actions">
        <!-- 未確認の行にバッジは出さない。既読管理はしていないので「NEW」は嘘になる
             （出していたのは「まだ確認済みにしていない」だけ）。それは左の赤い点と
             「確認済」バッジの有無で足りる -->
        ${reviewedBadge(d)}
        <!-- 1件だけ聞く。答えはそのままメモになる（回答を見せるモーダルは無い）。
             聞いている間は押せないようにする —— 状態は askingDomains に持つ（この行のDOMは
             再描画で作り直されるため） -->
        ${askingDomains.has(d.domain)
          ? `<button class="action-btn ask-ai-btn" disabled>${AI_ICON} 調べています…</button>`
          : `<button class="action-btn ask-ai-btn" data-domain="${escapeHtml(d.domain)}" onclick="askOne(this.dataset.domain)" title="メインのAIが、web検索とPi-holeの観測データからこのドメインを詳しく調べます（結果はメモになります）">${AI_ICON} 詳しく調べる</button>`
        }
        <!-- メモは確認済みかどうかに関わらず書ける（確認済みにしないと残せなかったのをやめた） -->
        <button class="action-btn edit-note-btn" data-domain="${escapeHtml(d.domain)}" data-note="${escapeHtml(d.note)}" onclick="openModal(this.dataset.domain, this.dataset.note)" title="${d.note ? 'メモを書き直す' : 'メモを書く'}">${EDIT_ICON}</button>
        <!-- data-note を必ず渡す。 渡さないと this.dataset.note が undefined になり、
             メモ欄が空のまま開いて「確認済みにする」がAIの書いたメモを空で上書きしていた -->
        ${!d.reviewed
          ? `<button class="action-btn ok-btn" data-domain="${escapeHtml(d.domain)}" onclick="markReviewed(this.dataset.domain)" title="調べて納得した">確認済みにする</button>`
          : `<button class="action-btn unreview-btn" data-domain="${escapeHtml(d.domain)}" onclick="unmarkReviewed(this.dataset.domain)">未確認に戻す</button>`
        }
      </div>
    </div>
  `).join('');

  // 開いている詳細も一緒に描き直す。一覧を描き直す経路は全部ここを通るので、
  // AIに聞いた・確認済みにした結果が詳細にも即座に映る（更新の呼び出しを散らさない）
  renderDetail();
}

// ---- チェックした行の一括操作 ----

function toggleSelect(checkbox) {
  const domain = checkbox.dataset.domain;
  if (checkbox.checked) selectedDomains.add(domain); else selectedDomains.delete(domain);
  renderSelection(filteredDomains());
}

function toggleSelectAll(checkbox) {
  // 対象は「表示中」だけ。フィルターで見えていない行まで選ぶと、
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

// バーの表示を一覧に合わせる。一覧から消えた行の選択は落とす ——
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

// チェックした行をまとめて確認済みにする。メモは送らない ——
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
    showToast('保存できませんでした', 'error');
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
  renderSelection(filteredDomains());
}

function openModal(domain, existingNote = '') {
  // 詳細から開いたときは詳細を閉じる（覆いを2枚重ねない）
  if (detailDomain) closeDetailModal();
  pendingDomain = domain;
  document.getElementById('modal-domain').textContent = domain;
  document.getElementById('modal-note').value = existingNote;
  // 既に確認済みなら「確認済みにする」は出さない（押しても何も変わらないボタンを置かない）
  const item = allDomains.find(d => d.domain === domain);
  const decided = !!(item && item.reviewed);
  document.getElementById('modal-review-btn').hidden = decided;
  document.getElementById('modal').style.display = 'flex';
  document.getElementById('modal-note').focus();
}

function closeModal() {
  document.getElementById('modal').style.display = 'none';
  pendingDomain = null;
}

// 覆いを押して閉じるのは mousedown（click ではない）。click は mousedown と mouseup が
// 同じ要素で起きたときに発火し、押した場所と離した場所が違えば共通の祖先で発火する ——
// モーダルの中の文章をドラッグで選んで覆いの上で指を離すと、覆いで click が起きて閉じてしまい、
// コピーしようとしただけで内容ごと消える。mousedown なら押した場所そのものが対象なので、
// 中から始めたドラッグは外で離しても閉じない。5つの覆いすべて同じ規則。
function onOverlayMouseDown(event) {
  if (event.target === document.getElementById('modal')) closeModal();
}

// ---- 行を押して開く詳細 ----
// 狭い画面ではここが本体。 行にドメイン・メモ・操作を全部並べると、幅の足りない画面で
// ドメインとメモが1文字ずつ縦に折り返して読めなくなる。行に出すのは要点だけにして、
// 全文（特に相手ごとに何行にもなるメモ）と操作はここで読ませる。

// 一覧は renderDomains() が innerHTML で作り直すので、リスナーは入れ物に1回だけ付ける
// （相手を選ぶモーダルの #ai-list と同じ流儀）
document.getElementById('domain-list').addEventListener('click', event => {
  // ボタン・チェックボックスを押したときは詳細を開かない —— 行のどこを押しても開くと、
  // 「AIに聞く」を押すたびに詳細まで開いてしまう
  if (event.target.closest('button, input, a, label, select, textarea')) return;
  const item = event.target.closest('.domain-item');
  if (item) openDetailModal(item.dataset.domain);
});

function openDetailModal(domain) {
  // 別のドメインを開いたら書きかけの質問は捨てる（前の行への質問を投げないため）
  if (detailDomain !== domain) followupDraft = '';
  detailDomain = domain;
  renderDetail();
  document.getElementById('detail-modal').style.display = 'flex';
}

function closeDetailModal() {
  document.getElementById('detail-modal').style.display = 'none';
  detailDomain = null;
}

function onDetailOverlayMouseDown(event) {
  if (event.target === document.getElementById('detail-modal')) closeDetailModal();
}

// 開いていなければ何もしない。renderDomains() から毎回呼ばれるので、
// 「開いているときだけ描き直す」の判定はここに1つ置く
function renderDetail() {
  if (!detailDomain) return;
  const d = allDomains.find(x => x.domain === detailDomain);
  // 一覧の読み直しで消えたドメイン（Pi-hole 側から落ちた等）は閉じる ——
  // 中身の無い詳細を開いたままにしない
  if (!d) { closeDetailModal(); return; }

  const asking = askingDomains.has(d.domain);
  document.getElementById('detail-body').innerHTML = `
    ${asking ? '<div class="detail-running">AIが調べています…（web検索を伴うので数十秒かかります）</div>' : ''}
    <div class="detail-domain">${escapeHtml(d.domain)}<button class="copy-btn detail-copy" data-domain="${escapeHtml(d.domain)}" onclick="copyDomain(this)" title="コピー">${COPY_ICON}</button></div>
    <div class="detail-meta">
      ${d.reviewed ? reviewedBadge(d) : '<span class="badge unreviewed">未確認</span>'}
      <span>${d.reasons ? '直近' : 'ブロック'} ${d.count.toLocaleString('ja-JP')} 回</span>
    </div>
    ${reasonsHtml(d)}
    ${activityHtml(d)}
    ${d.research ? `
      <div class="detail-label detail-label-row">
        <span>AIの調査結果${d.researched_at ? `<span class="detail-when">${escapeHtml(shortTime(d.researched_at))}</span>` : ''}</span>
        <button class="copy-btn detail-copy" data-domain="${escapeHtml(d.domain)}" onclick="copyResearch(this)" title="調査結果をコピー">${COPY_ICON}</button>
      </div>
      <div class="detail-research">${escapeHtml(d.research)}</div>
      <!-- 調べた結果をもとに、もう一歩聞く。 調査結果のすぐ下に置く ——
           読んで浮かんだ疑問をその場で投げられるのが要点で、離すと入力欄を探すことになる。
           調査結果が無いときは出さない（材料が無い深掘りは「詳しく調べる」の劣化版） -->
      <div class="followup">
        <textarea class="followup-input" id="followup-input" rows="2"
                  placeholder="この結果について、さらに聞く（例: 止めたら何が使えなくなる？）"
                  oninput="setFollowupDraft(this.value)" ${asking ? 'disabled' : ''}>${escapeHtml(followupDraft)}</textarea>
        <button class="action-btn ask-ai-btn followup-btn" onclick="askFollowup()" ${asking ? 'disabled' : ''}
                title="メインのAIに、これまでの調査結果と観測データを渡して追加で聞きます（Ctrl+Enter）">${AI_ICON} 追加で聞く</button>
      </div>
    ` : ''}
    <div class="detail-label">メモ</div>
    <div class="detail-note ${d.note ? '' : 'is-empty'}">${d.note ? escapeHtml(d.note) : 'まだメモはありません。'}</div>
  `;

  // 操作は行と同じ顔ぶれ。行から消さない（広い画面では行で完結するほうが速い）ので、
  // どちらから押しても同じ関数を通す
  document.getElementById('detail-actions').innerHTML = `
    <button class="action-btn cancel-btn" onclick="closeDetailModal()">閉じる</button>
    ${asking
      ? `<button class="action-btn ask-ai-btn" disabled>${AI_ICON} 調べています…</button>`
      : `<button class="action-btn ask-ai-btn" data-domain="${escapeHtml(d.domain)}" onclick="askOne(this.dataset.domain)">${AI_ICON} 詳しく調べる</button>`
    }
    <button class="action-btn edit-note-btn detail-edit" data-domain="${escapeHtml(d.domain)}" data-note="${escapeHtml(d.note)}" onclick="editNoteFromDetail(this.dataset.domain, this.dataset.note)">${EDIT_ICON} メモを書く</button>
    ${d.reviewed
      ? `<button class="action-btn unreview-btn" data-domain="${escapeHtml(d.domain)}" onclick="unmarkReviewed(this.dataset.domain)">未確認に戻す</button>`
      : `<button class="action-btn ok-btn" data-domain="${escapeHtml(d.domain)}" onclick="markReviewed(this.dataset.domain)">確認済みにする</button>`
    }
  `;
}

// メモの編集は既存のモーダルに渡す。詳細は先に閉じる ——
// 同じ z-index の覆いを2枚重ねると、後ろの詳細を押して閉じられてしまう
function editNoteFromDetail(domain, note) {
  closeDetailModal();
  openModal(domain, note);
}


// ---- 1件を詳しく調べる ----
// 「まとめてAIに聞く」とは役割が違う。 あちらは何十件ぶんの1〜2文のメモを
// 選んだ全員に書かせるもの。こちらはメインのAI1人に、web検索とPi-holeの観測データを
// 渡して1件を深く調べさせる。時間がかかる（web検索を伴うので数十秒〜数分）ぶん、
// 押している間の見た目は行ごとに保つ（`askingDomains`）。
// 結果はそのままメモになる（保存はサーバ側で済んでいる）。

async function askOne(domain) {
  // AIが1つも設定されていないなら、送る前に止めて設定へ案内する
  // （送っても必ず失敗するので、押した人には「動かない」としか見えない）
  if (!aiConfigured()) { openAiNotice(); return; }
  if (askingDomains.has(domain)) return;
  askingDomains.add(domain);
  renderDomains();

  let result;
  try {
    const resp = await fetch('/api/investigate', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      // どちらの一覧から押したかを渡す。 渡さないと、素通りしている通信まで
      // 「ブロックされたドメイン」として説明される。理由（観測した事実）も一緒に渡す
      body: JSON.stringify({
        domain,
        mode: currentMode,
        reason: reasonText(allDomains.find(d => d.domain === domain))
      })
    });
    result = await resp.json();
  } catch(e) {
    result = {success: false, error: '通信に失敗しました'};
  }
  askingDomains.delete(domain);

  if (result.error === 'token_required') {
    // トークンが要るのは同梱のCLIが相手のとき。設定のページへ案内する
    renderDomains();
    openAiNotice('トークンが未登録です。設定のページでトークンを登録するか、Chiezo の相手を選んでください。');
    return;
  }

  if (result.success && result.research) {
    const item = allDomains.find(d => d.domain === result.domain);
    if (item) {
      item.research = result.research;
      item.researched_at = result.researched_at || '';
      // メモが空だったときだけサーバが「ひとこと」を書いて返す。
      // 調べた結果は詳細画面でしか読めないので、それだけだと一覧に何も残らない。
      // 既にメモがあれば `note` は来ない（人の判断を上書きしないのはサーバ側の決まり）
      if (result.note) item.note = result.note;
    }
    renderDomains();
    // 調べ終わったら詳細を開く。 30秒待たせておいて結果をどこにも出さないと、
    // 押した人は何が起きたのか分からない（開いていれば renderDetail が描き直す）
    if (detailDomain !== domain) openDetailModal(domain);
    showToast(`${domain} を調べました（${result.author || 'AI'}）`
      + `${result.note ? ' — メモにも書きました' : ''}`, 'success');
    return;
  }

  renderDomains();
  showToast(`調べられませんでした（${result.error || '答えが返りませんでした'}）`, 'error');
}

// ---- 調査結果をもとに、もう一歩聞く ----
// 相手も材料も「詳しく調べる」と同じ（メインの1人・web検索・観測データ）で、
// 違うのはこれまでのやり取りと質問を渡すところ。答えは調査結果の末尾に足される
// —— 1つ目の答えとその続きが離れると読めないし、次の質問に渡す材料も組み立てにくい。
// 聞いている間の状態は `askingDomains` に相乗りする（「詳しく調べる」と同時に走らせない）

// 書きかけを覚える。関数を通す（インラインの属性から変数へ直に代入すると、
// 同じ名前の属性が要素側にあったときに黙ってそちらへ入る）
function setFollowupDraft(value) {
  followupDraft = value;
}

async function askFollowup() {
  const domain = detailDomain;
  if (!domain || askingDomains.has(domain)) return;
  const question = (followupDraft || '').trim();
  if (!question) { showToast('聞きたいことを入れてください', 'error'); return; }
  // AIが1つも設定されていないなら、送る前に止めて設定へ案内する
  // （送っても必ず失敗するので、押した人には「動かない」としか見えない）
  if (!aiConfigured()) { openAiNotice(); return; }

  askingDomains.add(domain);
  renderDomains();

  let result;
  try {
    const resp = await fetch('/api/followup', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      // mode と reason は「詳しく調べる」と同じものを渡す（材料を食い違わせない）
      body: JSON.stringify({
        domain,
        question,
        mode: currentMode,
        reason: reasonText(allDomains.find(d => d.domain === domain))
      })
    });
    result = await resp.json();
  } catch(e) {
    result = {success: false, error: '通信に失敗しました'};
  }
  askingDomains.delete(domain);

  if (result.error === 'token_required') {
    renderDomains();
    openAiNotice('トークンが未登録です。設定のページでトークンを登録するか、Chiezo の相手を選んでください。');
    return;
  }

  if (result.success && result.research) {
    const item = allDomains.find(d => d.domain === result.domain);
    if (item) {
      item.research = result.research;
      item.researched_at = result.researched_at || '';
    }
    // 聞けたら入力欄は空にする（同じ質問をもう一度投げないため）
    followupDraft = '';
    renderDomains();
    scrollResearchToBottom();
    showToast(`${result.author || 'AI'} が答えました`, 'success');
    return;
  }

  // 失敗しても質問は消さない（打ち直させない）
  renderDomains();
  showToast(`聞けませんでした（${result.error || '答えが返りませんでした'}）`, 'error');
}

// 追記した答えは調査結果の末尾＝入力欄のすぐ上に付く。入力欄を見えるところまで送る
// —— 詳細はモーダルごとスクロールするので、動かすのは箱の中ではなく外。
// 送らないと、描き直したときに前の内容が見えていて、答えが返ったのか分からない
function scrollResearchToBottom() {
  const el = document.getElementById('followup-input');
  if (el) el.scrollIntoView({block: 'center', behavior: 'smooth'});
}

// ---- まとめてAIに聞く ----
// いま一覧に出ているドメインを BULK_CHUNK 件ずつAIに聞き、結果をメモとして残す。
// 確認済みにはしない —— 調べただけの段階と、人が確認した段階は別

// 対象は未確認のうちメモが無いもの。空いているところを埋めるのが既定の動き ——
// メモがあるものまで毎回聞き直すと、同じ答えを取り直すために枠と時間を使う。
// 作り直しはチェックしたときだけ（`bulk-regenerate`）。
// 確認済みは触らない（人が確認したメモをAIの文章で上書きしないため）。
// フィルターに関係なく未確認を見る（表示を切り替えただけで対象が変わると分かりにくい）
function bulkTargets() {
  const regenerate = bulkRegenerate();
  return allDomains
    .filter(d => !d.reviewed)
    .filter(d => regenerate || !d.note)
    .map(d => d.domain);
}

// 「すでにメモがあるものも作り直す」か。覚えない ——
// 既定は「埋めるだけ」で、作り直しは押した回にだけ効いてほしい
function bulkRegenerate() {
  const box = document.getElementById('bulk-regenerate');
  return !!(box && box.checked);
}

// 1回の実行で聞く上限。覚えておく（毎回入れ直させない）
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
  // AIが1つも設定されていないなら、送る前に止めて設定へ案内する
  // （送っても必ず失敗するので、押した人には「動かない」としか見えない）
  if (!aiConfigured()) { openAiNotice(); return; }
  document.getElementById('bulk-limit').value = bulkLimit();

  const targets = bulkTargets();
  const limited = targets.slice(0, bulkLimit());
  const note = document.getElementById('bulk-note');
  note.className = 'bulk-note';
  const regenerate = bulkRegenerate();
  note.textContent = targets.length === 0
    ? (regenerate ? '未確認のドメインがありません。' : 'メモが空の未確認ドメインはありません（作り直すなら下のチェックを入れてください）。')
    : `${regenerate ? '未確認' : 'メモが空の未確認'} ${targets.length} 件のうち ${limited.length} 件を ${aiName()} に聞き、`
      + `結果をメモとして残します（確認済みの行は触りません）。`
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

function onBulkOverlayMouseDown(event) {
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

    // 候補に挙げた理由を一緒に渡す（監視モードのときだけ中身が入る）。
    // 対応表で渡す —— 並びで対応させると、サーバ側の重複・空白落としでずれる
    const reasons = {};
    for (const domain of chunk) {
      const text = reasonText(allDomains.find(d => d.domain === domain));
      if (text) reasons[domain] = text;
    }

    let result;
    try {
      const resp = await fetch('/api/ask', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        // どちらの一覧かを渡す。 ブロック済みと監視では聞くことが違う
        // （監視の候補に「ブロックされたと考えられます」と書かせない）
        body: JSON.stringify({domains: chunk, mode: currentMode, reasons})
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
      openAiNotice('トークンが未登録です。設定のページでトークンを登録するか、Chiezo の相手を選んでください。');
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
      // 1回の失敗で止めない（相手が一度崩れた応答を返しても、残りは聞ける）
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
}

// いま聞く相手の名前（複数）。取れていないときも空にしない
function aiNames() {
  return (aiState && aiState.current && aiState.current.length) ? aiState.current : ['AI'];
}

// 案内文に出す表記。全員を並べる
function aiName() {
  return aiNames().join(' / ');
}


// AIが1つも使える状態になっていないか。トークンも無く、Chiezoの相手も選んでいなければ
// 押しても必ず失敗するので、送る前に止めて設定へ案内する。
// Chiezoの相手は鍵をあちらが持っているので、選んでいればトークンは要らない
function aiConfigured() {
  if (!aiState) return true;   // 状態が取れないときは止めない（サーバの答えに任せる）
  if (aiState.token_saved) return true;
  return (aiState.selections || []).some(sel => sel.backend !== aiState.cli_backend);
}

// AIがまだ使えないことを伝える。設定はページへ移したので、ここでは理由と行き先だけ出す
function openAiNotice(message) {
  document.getElementById('ai-notice-text').textContent =
    message || 'まだ聞く相手が設定されていません。';
  document.getElementById('ai-notice-modal').style.display = 'flex';
}

function closeAiNotice() {
  document.getElementById('ai-notice-modal').style.display = 'none';
}

function onAiNoticeOverlayMouseDown(event) {
  if (event.target === document.getElementById('ai-notice-modal')) closeAiNotice();
}

// 「詳しく調べる」を頼む1人を選ぶラジオ。チェック（まとめて聞く相手）とは別の軸なので、
// 同じ行に並べて役割の違いを名前で示す
function primaryRadio(backend, current) {
  const checked = backend === current ? 'checked' : '';
  return `<label class="ai-row-primary" title="行の「詳しく調べる」を担当する1人">
    <input type="radio" name="ai-primary" value="${escapeHtml(backend)}" ${checked}> メイン
  </label>`;
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
    note.innerHTML = '<strong>チェック</strong>した相手ぜんぶに「まとめてAIに聞く」の内容を聞き、'
      + '答えを「誰が書いたか」付きで1つのメモに並べます。'
      + '<br><strong>メイン</strong>に選んだ1人だけが、行の「詳しく調べる」'
      + '（web検索とPi-holeの観測データで1件を深く調べる）を担当します。'
      + '<br>どちらも再起動なしで切り替わります。';
  }

  // 選択は複数。保存済みの選択から、相手ごとのモデル・考える量を引く
  const chosen = new Map((aiState.selections || []).map(sel => [sel.backend, sel]));
  // 先頭は従来の経路。消さずに残す —— Chiezoが落ちている日にも聞けるようにするため。
  // これも選択肢の1つなので、Chiezoの相手と一緒に選べる（両方に聞いて読み比べられる）
  const rows = [`
    <div class="ai-row">
      <label class="ai-row-main">
        <input type="checkbox" name="ai-backend" value="${escapeHtml(aiState.cli_backend)}"
               ${chosen.has(aiState.cli_backend) ? 'checked' : ''}>
        <span class="ai-row-name">${escapeHtml(aiState.cli_label)}</span>
      </label>
      ${primaryRadio(aiState.cli_backend, aiState.primary)}
      <!-- トークンの設定はこの行の中。値は出さない（登録済みかどうかだけ隣に出す） -->
      <div class="ai-row-opts">
        <span class="ai-row-hint">トークン: ${aiState.token_saved ? '登録済み' : '未登録'}</span>
        <input type="password" class="ai-token" id="token-input" autocomplete="off"
               placeholder="${aiState.token_saved ? '入れ替えるとき貼り付け' : 'トークンを貼り付け'}">
        <button type="button" class="action-btn review-btn" onclick="saveToken()">保存</button>
        ${aiState.token_saved
          ? '<button type="button" class="action-btn unreview-btn" onclick="deleteToken()" title="この端末から消す（入れ直せば元に戻る）">削除</button>'
          : ''}
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
          <span class="ai-row-name">${escapeHtml(backend.label)}${backend.web ? '' : '<span class="ai-row-hint"> web検索なし</span>'}</span>
        </label>
        ${primaryRadio(backend.id, aiState.primary)}
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

function setAiStatus(message, kind) {
  const el = document.getElementById('ai-status');
  el.textContent = message;
  el.className = `settings-status ${kind || ''}`;
}

async function saveAiSelection() {
  // チェックした全員を送る。 0人なら「未選択」= CLIブリッジに戻る
  const primary = document.querySelector('input[name="ai-primary"]:checked');
  const selections = [...document.querySelectorAll('input[name="ai-backend"]:checked')].map(box => {
    const backend = box.value;
    const value = role => {
      const el = document.querySelector(`select[data-role="${role}"][data-backend="${backend}"]`);
      return el ? el.value : '';
    };
    return {backend, model: value('model'), effort: value('effort'),
            primary: !!primary && primary.value === backend};
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
      // 保存した値そのものを画面へ反映する（Chiezoへ聞き直さない）
      await loadAi();
      renderAiList();
      setAiStatus(`${(result.current || []).join(' / ')} に聞きます`, 'ok');
      showToast('聞く相手を保存しました', 'success');
    } else {
      // 失敗の理由はその場に残す（トーストは消える）
      note.className = 'ai-note error-text';
      note.textContent = result.error || '保存に失敗しました';
      setAiStatus('保存できませんでした', 'error');
    }
  } catch(e) {
    note.className = 'ai-note error-text';
    note.textContent = '保存に失敗しました';
  }
}

// 保存したトークンを消す。入れ直せば元に戻るが、押し間違いだと次に聞いたときまで
// 気づけないので確認を挟む
async function deleteToken() {
  if (!confirm('保存したトークンを削除します。よろしいですか？（入れ直せば元に戻ります）')) return;
  try {
    const resp = await fetch('/api/claude-token', {method: 'DELETE'});
    const result = await resp.json();
    if (result.success) {
      await loadAi();
      renderAiList();
      setAiStatus('トークンを削除しました', 'ok');
      showToast('トークンを削除しました', 'success');
    } else {
      setAiStatus(result.error || 'トークンを削除できませんでした', 'error');
    }
  } catch(e) {
    setAiStatus('トークンを削除できませんでした', 'error');
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
      // 「登録済み」の表示を取り直す。保存の副作用でAIを呼ばない
      // （枠を使う操作は、押した人が改めて押して始める）
      await loadAi();
      renderAiList();
      setAiStatus('トークンを登録しました', 'ok');
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

// ---- 設定（ネットワークの確認） ----
// 一覧の判定には関わらない道具。 一覧に並ぶのは「名前を引いた記録」だけなので、
// その先に本当に届くのかは分からない —— 実際にパケットを出して確かめる場所を用意する。
// 相手先の確かめとコマンドの組み立てはサーバ側（src/diag.rs）。画面は文字を渡すだけ

// ---- ページとモードの切り替え（ドメイン / 設定・ブロック済み / 未ブロックの監視）----
// どちらも URL のハッシュで持つ（`#domains/watch` の形）。 戻る/進むとブックマークを
// そのまま効かせるため（JS でボタンを押し替える作りだと、どちらも効かない）。
// モードも URL に入れる。 変数だけで持っていた頃は、監視を見ている最中に
// 読み直す（下に引っ張って更新・PWA の復帰）とブロック済みへ戻っていた ——
// 押した覚えの無い切り替わり方で、しかも一覧が似ているので気づきにくい。
// 知らない値はそれぞれ既定に倒す —— 白い画面を出さない
const PAGES = ['domains', 'settings'];
const MODES = ['blocked', 'watch'];
let currentPage = 'domains';

function routeFromHash() {
  const [page, mode] = (location.hash || '').replace(/^#/, '').split('/');
  return {
    page: PAGES.includes(page) ? page : 'domains',
    // モードの無いハッシュ（`#domains` / `#settings`）ではモードを変えない ——
    // 設定を開いただけで一覧が入れ替わるのは、押した覚えの無い切り替わり方
    mode: MODES.includes(mode) ? mode : null,
  };
}

// `initial` は最初の1回。 モードが決まっても読み直さない ——
// この直後に `loadDomains()` を1回呼ぶので、同じ一覧を二重に取りに行かないため
function applyRoute(initial) {
  const route = routeFromHash();
  const modeChanged = route.mode !== null && route.mode !== currentMode;
  if (modeChanged) {
    currentMode = route.mode;
    // モードを跨いだ選択は残さない。 別の一覧の行を選んだまま「選択を確認済みにする」を
    // 押すと、見えていないものを確認済みにすることになる
    selectedDomains.clear();
  }
  currentPage = route.page;
  for (const name of PAGES) {
    document.getElementById(`page-${name}`).hidden = name !== currentPage;
    document.getElementById(`page-btn-${name}`).classList.toggle('active', name === currentPage);
  }
  // ヘッダーの「ドメイン」はいま見ているモードへ戻す —— `#domains` へ戻すと、
  // 設定から帰ってきて読み直したときにタブが入れ替わる
  document.getElementById('page-btn-domains').href = `#domains/${currentMode}`;
  for (const mode of MODES) {
    document.getElementById(`mode-${mode}`).classList.toggle('active', mode === currentMode);
  }
  if (modeChanged && !initial) loadDomains();
  // 設定を開いたときに読み直す。 別の端末で変えた値のまま保存すると上書きになるし、
  // Chiezoを後から起動した場合に、画面を読み直さなくても相手が出てくる
  if (currentPage === 'settings') {
    loadSettings();
    loadClients();
    loadNotes();
    setAiStatus('');
    loadAi().then(renderAiList);
  }
}

window.addEventListener('hashchange', () => applyRoute());

// ---- 設定：アクセス元（端末別） ----
// 一覧は「どのドメインか」で並んでいるので、「どの端末が喋っているか」はここでしか見られない。
// 材料は dns_client_daily（生のクエリが消えても残す表）なので、保持期間より長く遡れる。
// 日ごとに出すのは、1日の数字だけでは増えたのか減ったのかが言えないため

// 出す日数。サーバ側の CLIENT_DAYS_DEFAULT と同じ値にすること
const CLIENT_DAYS = 14;

function setClientsStatus(message, kind) {
  const el = document.getElementById('clients-status');
  el.textContent = message;
  el.className = `settings-status ${kind || ''}`;
}

async function loadClients() {
  const box = document.getElementById('client-table');
  box.innerHTML = '<div class="loading">読み込み中...</div>';
  try {
    const resp = await fetch(`/api/clients?days=${CLIENT_DAYS}`);
    renderClients(await resp.json());
  } catch(e) {
    box.innerHTML = '';
    setClientsStatus('アクセス元を読み込めませんでした', 'error');
  }
}

// 日付の列見出し。「08-19」を「8/19」に詰める（列が何本も並ぶので字数を削る）
function shortDay(day) {
  const m = /^\d{4}-(\d{2})-(\d{2})$/.exec(day);
  return m ? `${Number(m[1])}/${Number(m[2])}` : day;
}

function renderClients(body) {
  const days = body.days || [];
  const items = body.items || [];
  const box = document.getElementById('client-table');
  const all = items.reduce((sum, x) => sum + x.total, 0);
  setClientsStatus(`${items.length} 個のアクセス元 / 累計 ${all.toLocaleString('ja-JP')} 件`);
  if (!items.length) {
    box.innerHTML = '<div class="empty-note">まだ記録がありません。</div>';
    return;
  }
  // 割合は全期間の合計に対するもの（列に出している日数ぶんではない）——
  // 「ルーター経由がどれだけを占めているか」を見るための数字なので、
  // 表に出す日数を変えたら意味が変わる、という作りにしない
  const rows = items.map(x => `
    <tr>
      <td class="client-ip">${escapeHtml(x.client)}</td>
      <td class="client-num">${x.total.toLocaleString('ja-JP')}</td>
      <td class="client-num">${all ? (x.total / all * 100).toFixed(1) : '0.0'}%</td>
      ${x.counts.map(n => `<td class="client-num${n ? '' : ' is-zero'}">${n ? n.toLocaleString('ja-JP') : '·'}</td>`).join('')}
    </tr>`).join('');
  box.innerHTML = `
    <table class="client-table">
      <thead>
        <tr>
          <th>アクセス元</th><th class="client-num">累計</th><th class="client-num">割合</th>
          ${days.map(d => `<th class="client-num">${escapeHtml(shortDay(d))}</th>`).join('')}
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

// ---- 設定：メモが残っているドメイン ----
// 一覧（ブロック済み・未ブロックの監視）はどちらも「いま出ているもの」しか並べない ——
// ブロック済みは Pi-hole の集計そのもの、監視は見ている窓の中だけ。
// 静かになったドメインのメモはどちらにも出てこないので、控えをここに置く。
// 並べるだけで、編集も削除もしない（直すのは一覧の側から）

function setNotesStatus(message, kind) {
  const el = document.getElementById('notes-status');
  el.textContent = message;
  el.className = `settings-status ${kind || ''}`;
}

// 1ページの件数。サーバ側の NOTES_LIMIT_DEFAULT と同じ値にすること
// （画面が送らなければサーバがこの数で切る）
const NOTES_PAGE = 50;
// いま何件目から見ているか。ページを跨いで持つ（「読み直す」でも同じ場所に留まる）
let notesOffset = 0;

// `offset` を渡すとそのページへ移る。省略するといまのページを読み直す
async function loadNotes(offset) {
  if (typeof offset === 'number') notesOffset = Math.max(0, offset);
  const list = document.getElementById('note-list');
  list.innerHTML = '<div class="loading">読み込み中...</div>';
  try {
    const resp = await fetch(`/api/notes?offset=${notesOffset}&limit=${NOTES_PAGE}`);
    const body = await resp.json();
    // 消えた行があって行き過ぎていたら先頭へ戻す ——
    // 「まだ56件あるのに空のページ」を出さないため
    if (!(body.items || []).length && body.total > 0 && notesOffset > 0) {
      return loadNotes(0);
    }
    notesOffset = body.offset || 0;
    renderNotes(body);
  } catch(e) {
    list.innerHTML = '';
    document.getElementById('note-nav').innerHTML = '';
    setNotesStatus('メモの一覧を読み込めませんでした', 'error');
  }
}

function renderNotes(body) {
  const items = body.items || [];
  const total = body.total || 0;
  const offset = body.offset || 0;
  const list = document.getElementById('note-list');
  setNotesStatus(items.length
    ? `${offset + 1}–${offset + items.length} / ${total} 件`
    : `${total} 件`);
  renderNotesNav(items.length, total, offset);
  if (!items.length) {
    list.innerHTML = '<div class="empty-note">まだメモはありません。</div>';
    return;
  }
  // ページを移ったら控えの頭から読ませる（中でスクロールする箱なので、
  // 前のページで下まで送った位置が残ると、次のページが途中から出てくる）
  list.scrollTop = 0;
  // 調査結果は畳んでおく（1件で何十行にもなるので、開いたまま並べると控えを見渡せない）。
  // メモが空でも調査結果や確認済みだけの行はあるので、行ごと落とさない
  list.innerHTML = items.map(n => `
    <div class="note-item">
      <div class="note-item-head">
        <span class="note-domain">${escapeHtml(n.domain)}</span>
        <span class="badge ${n.reviewed ? 'reviewed' : 'unreviewed'}">${n.reviewed ? '確認済' : '未確認'}</span>
        <span class="note-when">${escapeHtml(shortTime(n.updated_at))}</span>
        <button class="copy-btn" data-domain="${escapeHtml(n.domain)}" onclick="copyDomain(this)" title="コピー">${COPY_ICON}</button>
      </div>
      ${n.note
        ? `<div class="domain-note">${escapeHtml(n.note)}</div>`
        : '<div class="domain-note is-empty">（メモなし）</div>'}
      ${n.research ? `
        <details class="note-research">
          <summary>AIの調査結果${n.researched_at ? `（${escapeHtml(shortTime(n.researched_at))}）` : ''}</summary>
          <div class="detail-research">${escapeHtml(n.research)}</div>
        </details>` : ''}
    </div>
  `).join('');
  list.scrollTop = 0;
}

// ページ送り。控えの下に置く（読み終わった先に次への入口がある）。
// 行き先が無いボタンは disabled にする —— 押しても何も起きないボタンを残すと、
// 最後まで来たのか読み込みに失敗したのかが押した人に分からない
function renderNotesNav(shown, total, offset) {
  const nav = document.getElementById('note-nav');
  if (total <= NOTES_PAGE) { nav.innerHTML = ''; return; }
  const first = offset <= 0;
  const last = offset + shown >= total;
  nav.innerHTML = `
    <button class="action-btn cancel-btn" onclick="loadNotes(notesOffset - NOTES_PAGE)" ${first ? 'disabled' : ''}>前へ</button>
    <span class="note-page">${Math.floor(offset / NOTES_PAGE) + 1} / ${Math.ceil(total / NOTES_PAGE)} ページ</span>
    <button class="action-btn cancel-btn" onclick="loadNotes(notesOffset + NOTES_PAGE)" ${last ? 'disabled' : ''}>次へ</button>
  `;
}

// ---- 設定：Pi-holeから取り込む件数 ----

function setQueryLimitStatus(message, kind) {
  const el = document.getElementById('query-limit-status');
  el.textContent = message;
  el.className = `settings-status ${kind || ''}`;
}

async function loadSettings() {
  try {
    const resp = await fetch('/api/settings');
    const data = await resp.json();
    document.getElementById('query-limit').value = data.query_limit;
    setQueryLimitStatus('');
  } catch(e) {
    setQueryLimitStatus('設定を読み込めませんでした', 'error');
  }
}

async function saveQueryLimit() {
  const raw = document.getElementById('query-limit').value.trim();
  const value = Number(raw);
  // 数でないものは送らない（サーバ側でも断るが、押した直後に理由が出るほうが早い）
  if (!raw || !Number.isInteger(value)) {
    setQueryLimitStatus('整数で入れてください（-1 で全件）', 'error');
    return;
  }
  const btn = document.getElementById('query-limit-save');
  btn.disabled = true;
  try {
    const resp = await fetch('/api/settings', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({query_limit: value})
    });
    const result = await resp.json();
    if (result.success) {
      setQueryLimitStatus(value === -1 ? '保存しました（全件を取り込みます）' : `保存しました（${value} 件まで）`, 'ok');
      showToast('設定を保存しました', 'success');
    } else {
      setQueryLimitStatus(result.error || '保存できませんでした', 'error');
    }
  } catch(e) {
    setQueryLimitStatus('通信に失敗しました', 'error');
  }
  btn.disabled = false;
}

// ---- 疎通の確認（ping / 経路）----
// 打っている間は両方のボタンを止める（同時に走らせない）
function setDiagRunning(running) {
  document.getElementById('diag-ping-btn').disabled = running;
  document.getElementById('diag-trace-btn').disabled = running;
}

// 途中経過を1行ずつ出す（NDJSON）。ping は4回・経路は応答しないホップがあると
// 数十秒かかるので、終わるまで待つと「打てているのか」が画面から分からない。
//
// 行は配列で持ち直す。 名前（`name`）は経路が出そろってから届いて既に出した行に
// 足されるので、文字列に継ぎ足していく作りだと書き足す先を指せない。
async function runDiag(tool) {
  const target = document.getElementById('diag-target').value.trim();
  const commandBox = document.getElementById('diag-command');
  const outputBox = document.getElementById('diag-output');
  if (!target) { showToast('相手先を入れてください', 'error'); return; }

  setDiagRunning(true);
  commandBox.hidden = false;
  commandBox.textContent = `${tool === 'ping' ? 'ping' : '経路'} を ${target} に打っています…`;
  outputBox.hidden = true;
  outputBox.textContent = '';

  const lines = [];
  const names = [];
  let command = '';

  // 名前はIPのすぐ後ろに差し込む。 行末に足すと、桁を揃えるための空白のぶんだけ
  // 右へ流れて画面の外に出る（出力の箱は横スクロールなので、横に送らないと読めない。
  // 実測：名前は68字目から始まるが、設定のモーダルに入るのは62字ほど）。
  // 続く空白を名前のぶんだけ詰めるので、応答時間の桁はたいてい動かない
  // —— tracepath はIPの後ろに40字ほど空けている
  const withName = (text, i) => {
    const hop = names[i];
    if (!hop) return text;
    const at = text.indexOf(hop.ip);
    const label = ` (${hop.name})`;
    if (at < 0) return `${text}${label}`;
    const head = text.slice(0, at + hop.ip.length);
    let rest = text.slice(at + hop.ip.length);
    for (let eaten = 0; eaten < label.length && rest.startsWith(' '); eaten++) rest = rest.slice(1);
    return `${head}${label}${rest}`;
  };

  const render = () => {
    // 一番下を見ていたときだけ追いかける（上を読み返している最中に引き戻さない）
    const atBottom = outputBox.scrollTop + outputBox.clientHeight >= outputBox.scrollHeight - 4;
    outputBox.hidden = false;
    outputBox.textContent = lines.map(withName).join('\n');
    if (atBottom) outputBox.scrollTop = outputBox.scrollHeight;
  };

  const apply = event => {
    if (event.t === 'start') {
      command = event.command;
      commandBox.textContent = `$ ${command}（実行中…）`;
    } else if (event.t === 'line') {
      lines[event.index] = event.text;
      render();
    } else if (event.t === 'name') {
      names[event.index] = {ip: event.ip, name: event.name};
      render();
    } else if (event.t === 'end') {
      // 走らせたコマンドと、かかった時間を添える。終了コードが0でなくても結果は出す
      // （応答が無いのも結果のうち）
      commandBox.textContent = `$ ${command}（${(event.elapsed_ms / 1000).toFixed(1)}秒`
        + `${event.ok ? '' : ' / 応答なしか失敗'}）`;
      if (!lines.length) { outputBox.hidden = false; outputBox.textContent = '（出力はありませんでした）'; }
    } else if (event.t === 'error') {
      // 理由は画面に残す（トーストは消えるので、打ち直すときに読めない）
      commandBox.textContent = event.message;
    }
  };

  // 受け取ったぶんを行で切って読む。チャンクの切れ目は行の途中にも来るので、
  // 最後の改行までを処理して残りは次のチャンクへ持ち越す
  let buffer = '';
  const consume = chunk => {
    buffer += chunk;
    let nl;
    while ((nl = buffer.indexOf('\n')) >= 0) {
      const raw = buffer.slice(0, nl).trim();
      buffer = buffer.slice(nl + 1);
      if (!raw) continue;
      try { apply(JSON.parse(raw)); } catch(e) { /* 読めない行は捨てる（次の行で追いつく） */ }
    }
  };

  try {
    const resp = await fetch('/api/diag', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({tool, target})
    });
    if (!resp.ok) {
      // 打てなかった（相手先の形が悪い・コマンドが無い）。理由は400のJSONで来る
      const result = await resp.json().catch(() => ({}));
      commandBox.textContent = result.error || '打てませんでした';
      setDiagRunning(false);
      return;
    }
    if (resp.body && resp.body.getReader) {
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      for (;;) {
        const {done, value} = await reader.read();
        if (done) break;
        consume(decoder.decode(value, {stream: true}));
      }
      consume(decoder.decode());
    } else {
      // 途中経過が読めない環境（古いブラウザ）。打てないよりはよい ——
      // 全部届いてから同じ手順で流し込む
      consume(await resp.text());
    }
  } catch(e) {
    commandBox.textContent = '通信に失敗しました';
  }
  setDiagRunning(false);
}

// ---- 下に引っ張って更新 ----
// スマホで一番よく使う操作。 「更新」ボタンは上のツールバーにあるので、
// 一覧を下まで読んだ後だと戻る手間がかかる。
// 引っ張った量だけ印を降ろし、しきい値を超えて離したら読み直す。
// 一番上にいるときだけ反応させる（一覧の途中で下向きに動かしたら普通のスクロール）

const PULL_TRIGGER_PX = 70;   // これを超えて離したら読み直す
const PULL_MAX_PX = 110;      // これ以上は降ろさない（引っ張り続けても伸びない）
let pullStartY = null;
let pullDistance = 0;

function pullIndicator() {
  return document.getElementById('pull-indicator');
}

// 印を動かす。`ready` は「いま離せば更新される」状態
function renderPull(distance, ready) {
  const el = pullIndicator();
  el.style.transform = `translate(-50%, ${distance}px)`;
  el.classList.toggle('visible', distance > 0);
  el.classList.toggle('ready', ready);
  document.getElementById('pull-text').textContent = ready ? '離すと更新' : '引っ張って更新';
}

function resetPull() {
  pullStartY = null;
  pullDistance = 0;
  const el = pullIndicator();
  el.classList.remove('visible', 'ready', 'loading');
  el.style.transform = '';
}

// モーダルが開いているときは動かさない（中の文章を読むための操作を横取りしない）
function anyModalOpen() {
  return [...document.querySelectorAll('.modal-overlay')]
    .some(m => m.style.display !== 'none' && m.style.display !== '');
}

document.addEventListener('touchstart', e => {
  if (e.touches.length !== 1 || anyModalOpen()) return;
  // 一覧を出しているときだけ。 設定のページで引っ張っても読み直すものが無い
  if (currentPage !== 'domains') return;
  // 一番上にいるときだけ始める（途中から始めると普通のスクロールを邪魔する）
  if (window.scrollY > 0) return;
  pullStartY = e.touches[0].clientY;
  pullDistance = 0;
}, {passive: true});

document.addEventListener('touchmove', e => {
  if (pullStartY === null) return;
  const delta = e.touches[0].clientY - pullStartY;
  if (delta <= 0 || window.scrollY > 0) { resetPull(); return; }
  // 引っ張るほど重くする（そのまま動かすと指に貼り付いて行き過ぎる）
  pullDistance = Math.min(delta * 0.5, PULL_MAX_PX);
  // ブラウザ自前の引っ張り更新を止める。 止めないと二重に走る
  // （このリスナーは passive: false でないと preventDefault が効かない）
  if (e.cancelable) e.preventDefault();
  renderPull(pullDistance, pullDistance >= PULL_TRIGGER_PX);
}, {passive: false});

document.addEventListener('touchend', () => {
  if (pullStartY === null) return;
  const ready = pullDistance >= PULL_TRIGGER_PX;
  pullStartY = null;
  if (!ready) { resetPull(); return; }

  // 読み直している間は印を出したままにする（押した結果が見えないと二度引っ張られる）
  const el = pullIndicator();
  el.classList.add('loading');
  document.getElementById('pull-text').textContent = '更新しています…';
  el.style.transform = `translate(-50%, ${PULL_TRIGGER_PX}px)`;
  loadDomains().finally(resetPull);
}, {passive: true});

// 指が画面から外れた（着信など）ときに印が出たままにならないように
document.addEventListener('touchcancel', resetPull, {passive: true});

// `navigator.clipboard` は安全なコンテキスト（https か localhost）でしか使えない。
// このアプリは LAN の IP に http で開くのが普通なので、その場合 API 自体が存在せず、
// 押しても何も起きない（実際そうなっていた）。古い経路（一時的な textarea への
// 選択 + execCommand）に落として、どちらでもコピーできるようにする。
// それも駄目なら黙らずに理由を出す —— 押した結果が分からないのが一番困る
function copyText(btn, text) {
  const done = () => {
    btn.innerHTML = CHECK_ICON;
    btn.classList.add('copied');
    setTimeout(() => { btn.innerHTML = COPY_ICON; btn.classList.remove('copied'); }, 1500);
  };

  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(done, () => {
      if (!legacyCopy(text)) showToast('コピーできませんでした', 'error');
      else done();
    });
    return;
  }
  if (legacyCopy(text)) done();
  else showToast('コピーできませんでした（手で選択してください）', 'error');
}

// 安全なコンテキストでないときの経路。画面の外に置いた textarea を選んで実行する
// （見えるところに置くと一瞬ちらつく）
function legacyCopy(text) {
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.left = '-9999px';
  document.body.appendChild(area);
  area.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
  document.body.removeChild(area);
  return ok;
}

function copyDomain(btn) {
  copyText(btn, btn.dataset.domain);
}

// 調査結果をまるごとコピーする。本文は属性に載せない ——
// 見出し付きで何行にもなるので、一覧から引く（行の DOM を重くしない）
function copyResearch(btn) {
  const item = allDomains.find(d => d.domain === btn.dataset.domain);
  if (!item || !item.research) return;
  copyText(btn, item.research);
}

// 1件を確認済みにする。メモは送らない ——
// サーバ側が「渡されなければ触らない」ので、AIに聞いた結果や自分で書いたメモが消えない
async function markReviewed(domain) {
  try {
    const resp = await fetch('/api/review', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({domains: [domain]})
    });
    const result = await resp.json();
    if (result.success) {
      const item = allDomains.find(d => d.domain === domain);
      if (item) item.reviewed = true;
      updateStats();
      renderDomains();
      showToast(`${domain} を確認済みにしました`, 'success');
    } else {
      showToast('保存できませんでした', 'error');
    }
  } catch(e) {
    showToast('エラーが発生しました', 'error');
  }
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
  if (e.key === 'Escape') { closeModal(); closeDetailModal(); closeAiNotice(); closeBulkModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitNote();
    // 詳細の追加質問。書きかけがあるときだけ（開いているだけで反応させない）
    if (detailDomain && (followupDraft || '').trim()) askFollowup();
  }
});

// モデル・考える量をいじったら、その行を選んだものとして扱う（選び直しの手数を減らす）。
// 一覧はinnerHTMLで差し替えるので、リスナーは入れ物に1回だけ付ける
document.getElementById('ai-list').addEventListener('change', e => {
  // モデル・考える量をいじったら、その行を選んだものとして扱う（選び直しの手数を減らす）
  if (e.target.tagName === 'SELECT') {
    const box = document.querySelector(`input[name="ai-backend"][value="${e.target.dataset.backend}"]`);
    if (box) box.checked = true;
    return;
  }
  // メインに選んだ相手はチェックも立てる。 立てないと「メインなのに聞く相手に
  // 入っていない」状態を保存でき、サーバ側で選択から落ちてメインが別の人にずれる
  if (e.target.name === 'ai-primary') {
    const box = document.querySelector(`input[name="ai-backend"][value="${e.target.value}"]`);
    if (box) box.checked = true;
  }
});

renderTheme();
applyRoute(true);
loadDomains();
loadAi();
