from flask import Blueprint

bp = Blueprint("items", __name__)


@bp.route("/<int:item_id>", methods=["GET"])
def read_item(item_id):
    return {"id": item_id}
