package matrix

import (
	"net/http"
	"testing"
)

const (
	fastapiURL = "http://fastapi-svc:8000"
	flaskURL = "http://flask-svc:5000"
	expressURL = "http://express-svc:3000"
	nestURL = "http://nest-svc:3000"
	nextURL = "http://next-svc:3000"
	springURL = "http://spring-svc:8080"
	ginURL = "http://gin-svc:8080"
	chiURL = "http://chi-svc:8080"
	nethttpURL = "http://nethttp-svc:8080"
	axumURL = "http://axum-svc:3000"
	actixURL = "http://actix-svc:8080"
)

func TestFastapiItem(t *testing.T) {
	http.Get(fastapiURL + "/fastapi/items/42")
}

func TestFlaskItem(t *testing.T) {
	http.Get(flaskURL + "/flask/items/42")
}

func TestExpressItem(t *testing.T) {
	http.Get(expressURL + "/express/items/42")
}

func TestNestItem(t *testing.T) {
	http.Get(nestURL + "/nest/items/42")
}

func TestNextItem(t *testing.T) {
	http.Get(nextURL + "/api/next/items/42")
}

func TestSpringItem(t *testing.T) {
	http.Get(springURL + "/spring/items/42")
}

func TestGinItem(t *testing.T) {
	http.Get(ginURL + "/gin/items/42")
}

func TestChiItem(t *testing.T) {
	http.Get(chiURL + "/chi/items/42")
}

func TestNethttpItem(t *testing.T) {
	http.Get(nethttpURL + "/nethttp/items/42")
}

func TestAxumItem(t *testing.T) {
	http.Get(axumURL + "/axum/items/42")
}

func TestActixItem(t *testing.T) {
	http.Get(actixURL + "/actix/items/42")
}
