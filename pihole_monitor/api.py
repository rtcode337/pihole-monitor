from flask import Blueprint, jsonify, request

from .claude_client import ask_claude_about_domain, save_claude_token
from .db import delete_reviewed, get_reviewed_domains, mark_as_reviewed
from .pihole_client import get_blocked_domains

api_bp = Blueprint("api", __name__, url_prefix="/api")


@api_bp.route("/domains")
def domains():
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


@api_bp.route("/review", methods=["POST", "DELETE"])
def review():
    domain = request.json.get("domain")
    if not domain:
        return jsonify({"success": False, "error": "domain required"}), 400
    if request.method == "DELETE":
        delete_reviewed(domain)
    else:
        note = request.json.get("note", "")
        mark_as_reviewed(domain, note)
    return jsonify({"success": True})


@api_bp.route("/ask-claude", methods=["POST"])
def ask_claude():
    domain = request.json.get("domain")
    if not domain:
        return jsonify({"success": False, "error": "domain required"}), 400
    answer, error = ask_claude_about_domain(domain)
    if error:
        status = 401 if error == "token_required" else 502
        return jsonify({"success": False, "error": error}), status
    return jsonify({"success": True, "answer": answer})


@api_bp.route("/claude-token", methods=["POST"])
def claude_token():
    token = (request.json or {}).get("token", "").strip()
    if not token:
        return jsonify({"success": False, "error": "token required"}), 400
    save_claude_token(token)
    return jsonify({"success": True})
