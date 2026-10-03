package main

import "net/http"

func getItem(w http.ResponseWriter, r *http.Request) {}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /nethttp/items/{id}", getItem)
	http.ListenAndServe(":8080", mux)
}
