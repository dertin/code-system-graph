"""Order event worker fixture."""


def configure_worker(kafka_consumer):
    """Subscribe the worker to the order-created channel."""
    kafka_consumer.subscribe("orders.created")


def load_order(connection, order_id):
    """Read an order through an exact literal query."""
    return connection.execute("SELECT id, status FROM commerce.orders WHERE id = ?", (order_id,))
