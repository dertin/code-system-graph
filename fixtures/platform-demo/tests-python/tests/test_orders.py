import requests


def test_create_order() -> None:
    response = requests.post(
        "/api/orders",
        json={"sku": "example", "quantity": 1},
        timeout=5,
    )

    assert response.status_code == 201
