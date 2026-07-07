import os
import sqlite3
from datetime import datetime

from .config import DB_PATH


def get_db():
    os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)
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


def delete_reviewed(domain):
    conn = get_db()
    conn.execute("DELETE FROM reviewed_domains WHERE domain = ?", (domain,))
    conn.commit()
    conn.close()
