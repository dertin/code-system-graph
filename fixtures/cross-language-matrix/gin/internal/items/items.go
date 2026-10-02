package items

import "github.com/gin-gonic/gin"

func Register(rg *gin.RouterGroup) {
	rg.GET("/:id", getItem)
}

func getItem(c *gin.Context) {
	c.JSON(200, gin.H{"id": c.Param("id")})
}
