package main

import (
	"github.com/gin-gonic/gin"
	"example.com/gin/internal/items"
)

func health(c *gin.Context) {}

func main() {
	r := gin.Default()
	r.GET("/health", health)
	api := r.Group("/gin")
	items.Register(api.Group("/items"))
	r.Run()
}
