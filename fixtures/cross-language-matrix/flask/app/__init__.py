from flask import Flask

from app import items


def create_app():
    app = Flask(__name__)
    app.register_blueprint(items.bp, url_prefix="/flask/items")
    return app
