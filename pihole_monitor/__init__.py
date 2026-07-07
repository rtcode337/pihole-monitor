from flask import Flask

from .api import api_bp
from .db import init_db
from .pages import pages_bp


def create_app():
    app = Flask(__name__)
    init_db()
    app.register_blueprint(pages_bp)
    app.register_blueprint(api_bp)
    return app
