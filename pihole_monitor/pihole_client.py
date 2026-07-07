from collections import Counter

import requests

from .config import PIHOLE_BASE_URL, PIHOLE_PASSWORD, PIHOLE_QUERY_LIMIT


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
        return Counter(q["domain"] for q in queries if q.get("domain"))
    except Exception as e:
        print(f"API error: {e}")
        return None
