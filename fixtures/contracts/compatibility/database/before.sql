CREATE TABLE commerce.orders (
    id BIGINT PRIMARY KEY,
    customer_id BIGINT NOT NULL,
    total_cents BIGINT NOT NULL
);

CREATE INDEX orders_customer_idx ON commerce.orders (customer_id);
