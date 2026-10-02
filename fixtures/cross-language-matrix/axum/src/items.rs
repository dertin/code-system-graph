use axum::extract::Path;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router {
    Router::new().route("/items/:id", get(read_item))
}

async fn read_item(Path(id): Path<String>) -> String {
    id
}
