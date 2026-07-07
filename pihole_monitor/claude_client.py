import os
import subprocess

from .config import AUTH_ERROR_KEYWORDS, CLAUDE_TIMEOUT, CLAUDE_TOKEN_PATH


def get_claude_token():
    try:
        with open(CLAUDE_TOKEN_PATH, "r") as f:
            token = f.read().strip()
            return token or None
    except FileNotFoundError:
        return None


def save_claude_token(token):
    fd = os.open(CLAUDE_TOKEN_PATH, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(token.strip())


def clear_claude_token():
    try:
        os.remove(CLAUDE_TOKEN_PATH)
    except FileNotFoundError:
        pass


def is_auth_error(stderr_text):
    lowered = stderr_text.lower()
    return any(kw in lowered for kw in AUTH_ERROR_KEYWORDS)


def ask_claude_about_domain(domain):
    """Queries the headless Claude Code CLI for a plain-language explanation of a blocked domain.
    Returns (answer, error). error == "token_required" means the caller should prompt the user
    for a fresh `claude setup-token` value."""
    token = get_claude_token()
    if not token:
        return None, "token_required"

    prompt = (
        f"Pi-holeの広告/トラッキングブロックリストによってブロックされたドメイン「{domain}」について、"
        f"これがどのようなサービス・通信に関連するドメインで、なぜブロックリストに含まれている可能性が高いかを"
        f"日本語で3〜5行程度で簡潔に説明してください。"
    )
    env = dict(os.environ, CLAUDE_CODE_OAUTH_TOKEN=token)
    try:
        result = subprocess.run(
            ["claude", "-p", prompt, "--output-format", "text"],
            capture_output=True,
            text=True,
            timeout=CLAUDE_TIMEOUT,
            env=env,
        )
        if result.returncode != 0:
            err = result.stderr.strip() or "claude command failed"
            print(f"[ask-claude] returncode={result.returncode} stderr={err!r} stdout={result.stdout.strip()!r}")
            if is_auth_error(err):
                clear_claude_token()
                return None, "token_required"
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
