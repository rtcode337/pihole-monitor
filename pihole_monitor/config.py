import os

PIHOLE_BASE_URL = os.environ.get("PIHOLE_BASE_URL", "http://pihole:80")
PIHOLE_PASSWORD = os.environ.get("PIHOLE_PASSWORD", "")
PIHOLE_QUERY_LIMIT = int(os.environ.get("PIHOLE_QUERY_LIMIT", "-1"))
CLAUDE_TIMEOUT = int(os.environ.get("CLAUDE_TIMEOUT", "60"))
DB_PATH = "/data/monitor.db"
CLAUDE_TOKEN_PATH = "/data/claude_token"

AUTH_ERROR_KEYWORDS = (
    "invalid api key",
    "invalid bearer token",
    "authentication_error",
    "unauthorized",
    "please run",
    "/login",
    "oauth token has expired",
    "token has expired",
    "token expired",
    "401",
)
