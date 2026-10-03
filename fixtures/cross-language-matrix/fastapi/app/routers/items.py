from fastapi import APIRouter

router = APIRouter(prefix="/items")


@router.get("/{item_id}")
def read_item(item_id: int):
    return {"id": item_id}
