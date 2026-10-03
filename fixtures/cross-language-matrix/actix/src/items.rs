use actix_web::{web, HttpResponse};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/items/{id}", web::get().to(read_item));
}

async fn read_item(id: web::Path<String>) -> HttpResponse {
    HttpResponse::Ok().body(id.into_inner())
}
