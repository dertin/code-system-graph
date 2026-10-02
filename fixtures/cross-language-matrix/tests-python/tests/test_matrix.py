import httpx
import requests

FASTAPI_URL = "http://fastapi-svc:8000"
FLASK_URL = "http://flask-svc:5000"
EXPRESS_URL = "http://express-svc:3000"
NEST_URL = "http://nest-svc:3000"
NEXT_URL = "http://next-svc:3000"
SPRING_URL = "http://spring-svc:8080"
GIN_URL = "http://gin-svc:8080"
CHI_URL = "http://chi-svc:8080"
NETHTTP_URL = "http://nethttp-svc:8080"
AXUM_URL = "http://axum-svc:3000"
ACTIX_URL = "http://actix-svc:8080"


def test_fastapi_item():
    requests.get(f"{FASTAPI_URL}/fastapi/items/42")


def test_flask_item():
    httpx.get(f"{FLASK_URL}/flask/items/42")


def test_express_item():
    requests.get(f"{EXPRESS_URL}/express/items/42")


def test_nest_item():
    httpx.get(f"{NEST_URL}/nest/items/42")


def test_next_item():
    requests.get(f"{NEXT_URL}/api/next/items/42")


def test_spring_item():
    httpx.get(f"{SPRING_URL}/spring/items/42")


def test_gin_item():
    requests.get(f"{GIN_URL}/gin/items/42")


def test_chi_item():
    httpx.get(f"{CHI_URL}/chi/items/42")


def test_nethttp_item():
    requests.get(f"{NETHTTP_URL}/nethttp/items/42")


def test_axum_item():
    httpx.get(f"{AXUM_URL}/axum/items/42")


def test_actix_item():
    requests.get(f"{ACTIX_URL}/actix/items/42")


def test_missing_item():
    requests.get("http://localhost:9000/missing/items/42")


def test_health():
    requests.get("http://localhost:8080/health")


def test_public_status():
    requests.get("https://status.example.com/api/v2/status")
