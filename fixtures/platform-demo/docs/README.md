# Commerce platform

Owner: `owner:@commerce-platform`

The [orders API](service:infra:orders-api) provides
[`POST /api/orders`](http:POST:/api/orders), publishes
[`orders.created`](event:kafka::orders.created), and owns
[`commerce.orders`](table::commerce:orders).

The [order worker](repo:worker) consumes the event and reads the shared table.
