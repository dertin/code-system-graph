mod items;

use actix_web::{web, App, HttpServer};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(web::scope("/actix").configure(items::configure)))
        .bind(("0.0.0.0", 8080))?
        .run()
        .await
}
