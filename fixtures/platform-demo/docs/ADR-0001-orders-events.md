# ADR-0001: Publish order creation events

Status: Accepted

Owner: `owner:@commerce-platform`

The `service:orders-api` publishes `event:kafka::orders.created` after writing
`table::commerce:orders`.
