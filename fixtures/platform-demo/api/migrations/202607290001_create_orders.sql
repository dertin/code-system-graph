CREATE TABLE commerce.orders (
    id UUID PRIMARY KEY,
    status VARCHAR(32) NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE INDEX orders_status_idx ON commerce.orders (status);
