resource "aws_sns_topic" "orders_created" {
  name = "orders.created"
}

resource "aws_db_instance" "orders" {
  identifier     = "orders"
  engine         = "postgres"
  instance_class = "db.t4g.micro"
  password       = "never-persist"
  username       = "fixture"
}
