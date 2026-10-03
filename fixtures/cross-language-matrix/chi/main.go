package main

import (
	"net/http"

	"github.com/go-chi/chi/v5"
)

func getItem(w http.ResponseWriter, r *http.Request) {}

func health(w http.ResponseWriter, r *http.Request) {}

func main() {
	r := chi.NewRouter()
	r.Get("/health", health)
	r.Route("/chi/items", func(r chi.Router) {
		r.Get("/{id}", getItem)
	})
	http.ListenAndServe(":8080", r)
}
