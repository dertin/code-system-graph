const FASTAPI_URL: &str = "http://fastapi-svc:8000";
const FLASK_URL: &str = "http://flask-svc:5000";
const EXPRESS_URL: &str = "http://express-svc:3000";
const NEST_URL: &str = "http://nest-svc:3000";
const NEXT_URL: &str = "http://next-svc:3000";
const SPRING_URL: &str = "http://spring-svc:8080";
const GIN_URL: &str = "http://gin-svc:8080";
const CHI_URL: &str = "http://chi-svc:8080";
const NETHTTP_URL: &str = "http://nethttp-svc:8080";
const AXUM_URL: &str = "http://axum-svc:3000";
const ACTIX_URL: &str = "http://actix-svc:8080";

#[tokio::test]
async fn reads_fastapi_item() {
    reqwest::get(format!("{FASTAPI_URL}/fastapi/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_flask_item() {
    reqwest::get(format!("{FLASK_URL}/flask/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_express_item() {
    reqwest::get(format!("{EXPRESS_URL}/express/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_nest_item() {
    reqwest::get(format!("{NEST_URL}/nest/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_next_item() {
    reqwest::get(format!("{NEXT_URL}/api/next/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_spring_item() {
    reqwest::get(format!("{SPRING_URL}/spring/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_gin_item() {
    reqwest::get(format!("{GIN_URL}/gin/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_chi_item() {
    reqwest::get(format!("{CHI_URL}/chi/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_nethttp_item() {
    reqwest::get(format!("{NETHTTP_URL}/nethttp/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_axum_item() {
    reqwest::get(format!("{AXUM_URL}/axum/items/42")).await.unwrap();
}

#[tokio::test]
async fn reads_actix_item() {
    reqwest::get(format!("{ACTIX_URL}/actix/items/42")).await.unwrap();
}
