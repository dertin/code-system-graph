mod items;

use axum::Router;

#[tokio::main]
async fn main() {
    let app = Router::new().nest("/axum", items::router());
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
