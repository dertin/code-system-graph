use axum::{Router, routing::post};

/// Builds the HTTP router used by the fixture.
pub fn router() -> Router {
    Router::new().route("/api/orders", post(create_order))
}

/// Creates an order.
pub async fn create_order() -> &'static str {
    "created"
}

/// Minimal Kafka publisher contract used by the fixture.
pub trait KafkaProducer {
    /// Publishes a payload to an exact channel.
    fn send(&self, channel: &str, payload: &str);
}

/// Publishes the order-created integration event.
pub fn publish_order_created(producer: &impl KafkaProducer) {
    producer.send("orders.created", r#"{"orderId":"fixture"}"#);
}
