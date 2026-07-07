from flask import Flask, jsonify, request, render_template_string
import sqlite3
import requests
import subprocess
import os
from datetime import datetime

app = Flask(__name__)

PIHOLE_BASE_URL = os.environ.get("PIHOLE_BASE_URL", "http://pihole:80")
PIHOLE_PASSWORD = os.environ.get("PIHOLE_PASSWORD", "")
PIHOLE_QUERY_LIMIT = int(os.environ.get("PIHOLE_QUERY_LIMIT", "-1"))
CLAUDE_TIMEOUT = int(os.environ.get("CLAUDE_TIMEOUT", "60"))
DB_PATH = "/data/monitor.db"

def get_db():
    conn = sqlite3.connect(DB_PATH)
    conn.row_factory = sqlite3.Row
    return conn

def init_db():
    conn = get_db()
    conn.execute("""
        CREATE TABLE IF NOT EXISTS reviewed_domains (
            domain TEXT PRIMARY KEY,
            reviewed_at TEXT NOT NULL,
            note TEXT
        )
    """)
    conn.commit()
    conn.close()

def get_pihole_token():
    try:
        resp = requests.post(
            f"{PIHOLE_BASE_URL}/api/auth",
            json={"password": PIHOLE_PASSWORD},
            timeout=5
        )
        data = resp.json()
        return data.get("session", {}).get("sid")
    except Exception as e:
        print(f"Auth error: {e}")
        return None

def get_blocked_domains():
    """Returns a Counter of blocked domains, or None if Pi-hole could not be reached."""
    token = get_pihole_token()
    if not token:
        return None

    try:
        resp = requests.get(
            f"{PIHOLE_BASE_URL}/api/queries",
            params={"upstream": "blocklist", "length": PIHOLE_QUERY_LIMIT},
            headers={"sid": token},
            timeout=5
        )
        resp.raise_for_status()
        data = resp.json()
        queries = data.get("queries", [])
        from collections import Counter
        return Counter(q["domain"] for q in queries if q.get("domain"))
    except Exception as e:
        print(f"API error: {e}")
        return None

def get_reviewed_domains():
    conn = get_db()
    rows = conn.execute("SELECT domain, note FROM reviewed_domains").fetchall()
    conn.close()
    return {row["domain"]: row["note"] or "" for row in rows}

def mark_as_reviewed(domain, note=""):
    conn = get_db()
    conn.execute(
        "INSERT OR REPLACE INTO reviewed_domains (domain, reviewed_at, note) VALUES (?, ?, ?)",
        (domain, datetime.now().isoformat(), note)
    )
    conn.commit()
    conn.close()

def ask_claude_about_domain(domain):
    """Queries the headless Claude Code CLI for a plain-language explanation of a blocked domain.
    Returns (answer, error)."""
    prompt = (
        f"Pi-holeの広告/トラッキングブロックリストによってブロックされたドメイン「{domain}」について、"
        f"これがどのようなサービス・通信に関連するドメインで、なぜブロックリストに含まれている可能性が高いかを"
        f"日本語で3〜5行程度で簡潔に説明してください。"
    )
    try:
        result = subprocess.run(
            ["claude", "-p", prompt, "--output-format", "text"],
            capture_output=True,
            text=True,
            timeout=CLAUDE_TIMEOUT,
        )
        if result.returncode != 0:
            err = result.stderr.strip() or "claude command failed"
            print(f"[ask-claude] returncode={result.returncode} stderr={err!r} stdout={result.stdout.strip()!r}")
            return None, err
        answer = result.stdout.strip()
        if not answer:
            print("[ask-claude] empty stdout from claude command")
            return None, "empty response from claude"
        return answer, None
    except subprocess.TimeoutExpired:
        print(f"[ask-claude] timeout after {CLAUDE_TIMEOUT}s")
        return None, "timeout"
    except FileNotFoundError:
        print("[ask-claude] claude command not found")
        return None, "claude command not found"
    except Exception as e:
        print(f"[ask-claude] unexpected error: {e}")
        return None, str(e)

HTML = """
<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Pi-hole Monitor</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }

  body {
    font-family: 'SF Mono', 'Fira Code', 'Consolas', monospace;
    background: #0d1117;
    color: #c9d1d9;
    min-height: 100vh;
  }

  header {
    border-bottom: 1px solid #21262d;
    padding: 20px 32px;
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .logo {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #f85149;
    box-shadow: 0 0 8px #f85149;
  }

  header h1 {
    font-size: 14px;
    font-weight: 400;
    color: #8b949e;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  header h1 span { color: #c9d1d9; }

  .stats {
    display: flex;
    gap: 1px;
    padding: 0 32px;
    margin: 24px 0;
    background: #21262d;
    border-top: 1px solid #21262d;
    border-bottom: 1px solid #21262d;
  }

  .stat {
    flex: 1;
    padding: 16px 24px;
    background: #0d1117;
  }

  .stat-value {
    font-size: 28px;
    font-weight: 300;
    color: #f0f6fc;
    line-height: 1;
  }

  .stat-value.alert { color: #f85149; }
  .stat-value.ok { color: #58a6ff; }

  .stat-label {
    font-size: 11px;
    color: #8b949e;
    margin-top: 4px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .toolbar {
    padding: 0 32px;
    margin-bottom: 16px;
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .filter-btn {
    padding: 6px 14px;
    border: 1px solid #30363d;
    border-radius: 4px;
    background: transparent;
    color: #8b949e;
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
    letter-spacing: 0.04em;
    transition: all 0.15s;
  }

  .filter-btn.active {
    background: #21262d;
    color: #c9d1d9;
    border-color: #8b949e;
  }

  .refresh-btn {
    margin-left: auto;
    padding: 6px 14px;
    border: 1px solid #30363d;
    border-radius: 4px;
    background: transparent;
    color: #8b949e;
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
    transition: all 0.15s;
  }

  .refresh-btn:hover { color: #c9d1d9; border-color: #8b949e; }

  .domain-list {
    padding: 0 32px 32px;
  }

  .domain-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    border: 1px solid #21262d;
    border-radius: 6px;
    margin-bottom: 4px;
    transition: border-color 0.15s;
  }

  .domain-item:hover { border-color: #30363d; }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .status-dot.new { background: #f85149; box-shadow: 0 0 6px #f85149; }
  .status-dot.reviewed { background: #58a6ff; }

  .domain-info {
    flex: 1;
    min-width: 0;
  }

  .domain-name {
    font-size: 13px;
    color: #c9d1d9;
    word-break: break-all;
    position: relative;
    display: inline-block;
  }

  .domain-count {
    font-size: 11px;
    color: #484f58;
  }

  .domain-note {
    font-size: 11px;
    color: #8b949e;
    margin-top: 3px;
    word-break: break-all;
  }

  .badge {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .badge.reviewed {
    background: #1a2535;
    color: #58a6ff;
    border: 1px solid #388bfd;
  }

  .badge.new {
    background: #3d1f1f;
    color: #f85149;
    border: 1px solid #da3633;
  }

  .action-btn {
    padding: 4px 12px;
    border-radius: 4px;
    font-family: inherit;
    font-size: 11px;
    cursor: pointer;
    transition: all 0.15s;
    flex-shrink: 0;
    letter-spacing: 0.04em;
  }

  .review-btn {
    border: 1px solid #388bfd;
    background: transparent;
    color: #58a6ff;
  }

  .review-btn:hover { background: #1f3a5c; }

  .unreview-btn {
    border: 1px solid #30363d;
    background: transparent;
    color: #8b949e;
  }

  .unreview-btn:hover { color: #c9d1d9; border-color: #8b949e; }

  .ask-claude-btn {
    border: 1px solid #6e4a2e;
    background: transparent;
    color: #d9a066;
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .ask-claude-btn:hover { background: #3a2a1a; }

  .claude-modal-body {
    font-size: 12px;
    line-height: 1.7;
    color: #c9d1d9;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 40vh;
    overflow-y: auto;
    margin-bottom: 16px;
  }

  .claude-modal-body.loading-text {
    color: #8b949e;
  }

  .claude-modal-body.error-text {
    color: #f85149;
  }

  .edit-note-btn {
    border: 1px solid #30363d;
    background: transparent;
    color: #8b949e;
  }

  .edit-note-btn:hover { color: #c9d1d9; border-color: #8b949e; }

  .copy-btn {
    position: absolute;
    top: -6px;
    right: -16px;
    border: none;
    background: transparent;
    color: #484f58;
    cursor: pointer;
    padding: 0;
    display: flex;
    align-items: center;
    opacity: 0;
    transition: opacity 0.15s, color 0.15s;
  }

  .domain-item:hover .copy-btn { opacity: 1; }
  .copy-btn:hover { color: #8b949e; }
  .copy-btn.copied { color: #3fb950; opacity: 1; }

  .empty {
    text-align: center;
    padding: 64px;
    color: #8b949e;
    font-size: 13px;
  }

  .empty-icon {
    font-size: 32px;
    margin-bottom: 12px;
    opacity: 0.4;
  }

  .loading {
    text-align: center;
    padding: 64px;
    color: #8b949e;
    font-size: 12px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .toast {
    position: fixed;
    bottom: 24px;
    right: 24px;
    padding: 10px 16px;
    border-radius: 6px;
    font-size: 12px;
    opacity: 0;
    transition: opacity 0.3s;
    pointer-events: none;
  }

  .toast.show { opacity: 1; }
  .toast.success { background: #1a2535; color: #58a6ff; border: 1px solid #388bfd; }
  .toast.error { background: #3d1f1f; color: #f85149; border: 1px solid #da3633; }

  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal {
    background: #161b22;
    border: 1px solid #30363d;
    border-radius: 8px;
    padding: 24px;
    width: 480px;
    max-width: 90vw;
  }

  .modal-title {
    font-size: 11px;
    color: #8b949e;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 8px;
  }

  .modal-domain {
    font-size: 14px;
    color: #c9d1d9;
    margin-bottom: 16px;
    word-break: break-all;
  }

  .modal-note {
    width: 100%;
    background: #0d1117;
    border: 1px solid #30363d;
    border-radius: 4px;
    color: #c9d1d9;
    font-family: inherit;
    font-size: 12px;
    padding: 10px;
    resize: vertical;
    min-height: 80px;
    margin-bottom: 16px;
  }

  .modal-note::placeholder { color: #484f58; }
  .modal-note:focus { outline: none; border-color: #58a6ff; }

  .modal-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }

  .cancel-btn {
    border: 1px solid #30363d;
    background: transparent;
    color: #8b949e;
  }

  .cancel-btn:hover { color: #c9d1d9; border-color: #8b949e; }

  .confirm-btn {
    border: 1px solid #388bfd;
    background: #1f3a5c;
    color: #58a6ff;
  }

  .confirm-btn:hover { background: #264a7a; }
</style>
</head>
<body>

<header>
  <div class="logo"></div>
  <h1>Pi-hole <span>Monitor</span></h1>
</header>

<div class="stats">
  <div class="stat">
    <div class="stat-value alert" id="stat-new">-</div>
    <div class="stat-label">未確認</div>
  </div>
  <div class="stat">
    <div class="stat-value ok" id="stat-reviewed">-</div>
    <div class="stat-label">確認済み</div>
  </div>
  <div class="stat">
    <div class="stat-value" id="stat-total">-</div>
    <div class="stat-label">ブロック総数</div>
  </div>
</div>

<div class="toolbar">
  <button class="filter-btn active" onclick="setFilter('new', event)">未確認のみ</button>
  <button class="filter-btn" onclick="setFilter('reviewed', event)">確認済みのみ</button>
  <button class="filter-btn" onclick="setFilter('all', event)">すべて</button>
  <button class="refresh-btn" onclick="loadDomains()">更新</button>
</div>

<div class="domain-list" id="domain-list">
  <div class="loading">読み込み中...</div>
</div>

<div class="toast" id="toast"></div>

<div class="modal-overlay" id="modal" style="display:none" onclick="onOverlayClick(event)">
  <div class="modal">
    <div class="modal-title">確認済みにする</div>
    <div class="modal-domain" id="modal-domain"></div>
    <textarea id="modal-note" class="modal-note" placeholder="確認メモ（任意）"></textarea>
    <div class="modal-actions">
      <button class="action-btn cancel-btn" onclick="closeModal()">キャンセル</button>
      <button class="action-btn confirm-btn" onclick="submitReview()">確認済みにする</button>
    </div>
  </div>
</div>

<div class="modal-overlay" id="claude-modal" style="display:none" onclick="onClaudeOverlayClick(event)">
  <div class="modal">
    <div class="modal-title">Claudeに聞く</div>
    <div class="modal-domain" id="claude-modal-domain"></div>
    <div class="claude-modal-body" id="claude-modal-body"></div>
    <textarea id="claude-modal-note" class="modal-note" placeholder="確認メモ（任意）"></textarea>
    <div class="modal-actions">
      <button class="action-btn cancel-btn" onclick="closeClaudeModal()">閉じる</button>
      <button class="action-btn confirm-btn" onclick="submitReviewFromClaudeModal()">確認済みにする</button>
    </div>
  </div>
</div>

<script>
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
    } else {
      body.textContent = `Claudeへの問い合わせに失敗しました（${result.error || '不明なエラー'}）`;
      body.className = 'claude-modal-body error-text';
    }
  } catch(e) {
    body.textContent = 'Claudeへの問い合わせに失敗しました';
    body.className = 'claude-modal-body error-text';
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
  if (e.key === 'Escape') { closeModal(); closeClaudeModal(); }
  if (e.key === 'Enter' && e.ctrlKey) {
    if (document.getElementById('modal').style.display !== 'none') submitReview();
    if (document.getElementById('claude-modal').style.display !== 'none') submitReviewFromClaudeModal();
  }
});

loadDomains();
</script>
</body>
</html>
"""

@app.route("/")
def index():
    return render_template_string(HTML)

@app.route("/api/domains")
def api_domains():
    blocked = get_blocked_domains()
    if blocked is None:
        return jsonify({"error": "pihole_unavailable"}), 502
    reviewed = get_reviewed_domains()
    result = []
    seen = set()
    for domain, count in blocked.items():
        result.append({
            "domain": domain,
            "count": count,
            "reviewed": domain in reviewed,
            "note": reviewed.get(domain, "")
        })
        seen.add(domain)
    for domain, note in reviewed.items():
        if domain not in seen:
            result.append({
                "domain": domain,
                "count": 0,
                "reviewed": True,
                "note": note
            })
    result.sort(key=lambda x: (x["reviewed"], -x["count"]))
    return jsonify(result)

@app.route("/api/review", methods=["POST", "DELETE"])
def api_review():
    domain = request.json.get("domain")
    if not domain:
        return jsonify({"success": False, "error": "domain required"}), 400
    if request.method == "DELETE":
        conn = get_db()
        conn.execute("DELETE FROM reviewed_domains WHERE domain = ?", (domain,))
        conn.commit()
        conn.close()
    else:
        note = request.json.get("note", "")
        mark_as_reviewed(domain, note)
    return jsonify({"success": True})

@app.route("/api/ask-claude", methods=["POST"])
def api_ask_claude():
    domain = request.json.get("domain")
    if not domain:
        return jsonify({"success": False, "error": "domain required"}), 400
    answer, error = ask_claude_about_domain(domain)
    if error:
        return jsonify({"success": False, "error": error}), 502
    return jsonify({"success": True, "answer": answer})

if __name__ == "__main__":
    init_db()
    app.run(host="0.0.0.0", port=8888, debug=False)
