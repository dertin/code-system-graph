use crate::{
    SourceObservation, SourceRole, SymbolRef, UrlPart, parse_go_source, parse_java_source, parse_javascript_source, parse_typescript_source
};

type Consumer<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

fn consumers(observations: &[SourceObservation]) -> Vec<Consumer<'_>> {
    observations
        .iter()
        .filter(|item| item.role == SourceRole::Consumer)
        .map(|item| {
            (
                item.method.as_deref(),
                item.path.as_deref(),
                item.authority.as_deref(),
                item.symbol_name.as_deref(),
            )
        })
        .collect()
}

#[test]
fn typescript_clients_should_resolve_constants_templates_instances_and_wrappers() {
    let source = r#"
import axios from "axios";
const API = "/api/v1";
const api = axios.create({ baseURL: "http://orders:8080/api/" });
export async function getOrder(id: string): Promise<Order> {
  return fetch(`${API}/orders/${id}`).then((response) => response.json());
}
export const deleteOrder = async (id: string) => api.delete(`/orders/${encodeURIComponent(id)}`);
export function request(path: string, method: string) {
  return fetch(API + path, { method });
}
document.querySelector("a").addEventListener("click", () => {
  fetch("/first");
  fetch("/second", { method: "POST" });
});
"#;
    let result = parse_typescript_source(source);

    assert_eq!(
        consumers(&result),
        [
            (
                Some("GET"),
                Some("/api/v1/orders/{id}"),
                None,
                Some("getOrder")
            ),
            (
                Some("DELETE"),
                Some("/api/orders/{id}"),
                Some("orders:8080"),
                Some("deleteOrder")
            ),
            (None, None, None, Some("request")),
            (Some("GET"), Some("/first"), None, None),
            (Some("POST"), Some("/second"), None, None),
        ]
    );
    let wrapper = result
        .iter()
        .find(|item| item.symbol_name.as_deref() == Some("request"))
        .and_then(|item| item.url.as_ref())
        .map(|url| url.parts.clone());
    assert_eq!(
        wrapper,
        Some(vec![
            UrlPart::Text("/api/v1".to_owned()),
            UrlPart::Parameter {
                name: "path".to_owned(),
                index: 0
            },
        ])
    );
}

#[test]
fn typescript_test_files_should_record_relative_import_calls() {
    let source = r#"
import { createOrder } from "./helpers";
import * as api from "../api";
export async function seed() {
  await createOrder("/orders/1");
  await api.listOrders();
  localHelper();
}
describe("orders", () => {});
"#;
    let calls = parse_typescript_source(source)
        .into_iter()
        .filter(|item| item.role == SourceRole::Call)
        .filter_map(|item| item.call.map(|call| call.callee))
        .collect::<Vec<_>>();

    assert_eq!(
        calls,
        [
            SymbolRef::Import {
                module: "./helpers".to_owned(),
                name: "createOrder".to_owned()
            },
            SymbolRef::Import {
                module: "../api".to_owned(),
                name: "listOrders".to_owned()
            },
            SymbolRef::Local("localHelper".to_owned()),
        ]
    );
}

#[test]
fn go_clients_should_resolve_sprintf_constants_and_client_receivers() {
    let source = r#"
package users

import (
    "fmt"
    "net/http"
)

const base = "http://users:9000"

type Client struct {
    client *http.Client
}

func (c *Client) GetUser(id string) (*User, error) {
    url := fmt.Sprintf("%s/v1/users/%s", base, id)
    req, _ := http.NewRequest(http.MethodGet, url, nil)
    return c.do(req)
}

func (c *Client) Ping() error {
    _, err := c.client.Head(base + "/healthz")
    return err
}

func Post(body io.Reader) {
    http.Post("/v1/users", "application/json", body)
}
"#;
    let result = parse_go_source(source);

    assert_eq!(
        consumers(&result),
        [
            (
                Some("GET"),
                Some("/v1/users/{id}"),
                Some("users:9000"),
                Some("GetUser")
            ),
            (
                Some("HEAD"),
                Some("/healthz"),
                Some("users:9000"),
                Some("Ping")
            ),
            (Some("POST"), Some("/v1/users"), None, Some("Post")),
        ]
    );
}

#[test]
fn java_web_client_should_resolve_chain_methods_and_constants() {
    let source = r#"
import org.springframework.web.reactive.function.client.WebClient;

class OrdersClient {
    private static final String ORDERS = "/orders";

    Mono<Order> find(String id) {
        return client.get().uri(ORDERS + "/" + id).retrieve().bodyToMono(Order.class);
    }

    void create() {
        client.method(HttpMethod.POST).uri("/orders/{id}", 7).retrieve();
    }
}
"#;
    let result = parse_java_source(source);

    assert_eq!(
        consumers(&result),
        [
            (Some("GET"), Some("/orders/{id}"), None, Some("find")),
            (Some("POST"), Some("/orders/{id}"), None, Some("create")),
        ]
    );
}

/// Sorted tests, consumers, and local calls as `role framework symbol: detail`.
fn test_facts(observations: &[SourceObservation]) -> Vec<String> {
    let mut facts = observations
        .iter()
        .filter_map(|item| {
            let detail = match item.role {
                SourceRole::Test => String::new(),
                SourceRole::Consumer => format!(
                    "{} {}",
                    item.method.as_deref().unwrap_or("?"),
                    item.path.as_deref().unwrap_or("?")
                ),
                SourceRole::Call => match &item.call.as_ref()?.callee {
                    SymbolRef::Local(name) => format!("-> {name}"),
                    _ => return None,
                },
                _ => return None,
            };
            Some(format!(
                "{:?} {:?} {}: {detail}",
                item.role,
                item.framework,
                item.symbol_name.as_deref().unwrap_or("-")
            ))
        })
        .collect::<Vec<_>>();
    facts.sort();
    facts
}

#[test]
fn script_tests_should_be_named_by_describe_chain_and_reach_hooks_and_supertest() {
    let source = r#"
const request = require("supertest");
const app = require("../app");

describe("orders", () => {
  let agent;
  beforeEach(async () => {
    agent = request.agent(app);
    await fetch("/seed", { method: "POST" });
  });

  it("creates an order", async () => {
    await request(app).post("/orders").send({ sku: "A" });
  });

  describe("by id", function () {
    test.only(`reads one`, async () => {
      await agent.get("/orders/42");
    });
  });
});
"#;
    let facts = test_facts(&parse_javascript_source(source));

    assert_eq!(
        facts,
        [
            "Call Fetch orders > by id > reads one: -> orders > beforeEach",
            "Call Fetch orders > creates an order: -> orders > beforeEach",
            "Call Fetch orders > creates an order: -> request",
            "Consumer Fetch orders > beforeEach: POST /seed",
            "Consumer Supertest orders > by id > reads one: GET /orders/42",
            "Consumer Supertest orders > creates an order: POST /orders",
            "Test Jest orders > by id > reads one: ",
            "Test Jest orders > creates an order: ",
        ]
    );
}

#[test]
fn playwright_request_fixture_calls_should_belong_to_their_test() {
    let source = r#"
import { test, expect } from "@playwright/test";

test.describe("catalog", () => {
  test("lists products", async ({ request }) => {
    const response = await request.get("/api/products");
    expect(response.ok()).toBeTruthy();
  });
});
"#;
    let facts = test_facts(&parse_typescript_source(source));

    assert_eq!(
        facts,
        [
            "Consumer Playwright catalog > lists products: GET /api/products",
            "Test Playwright catalog > lists products: ",
        ]
    );
}

#[test]
fn go_tests_should_recognize_httptest_requests_and_servers() {
    let source = r#"
package orders

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestGetOrder(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "/orders/42", nil)
	rec := httptest.NewRecorder()
	router().ServeHTTP(rec, req)
}

func TestCreateOrder(t *testing.T) {
	srv := httptest.NewServer(router())
	defer srv.Close()
	http.Post(srv.URL+"/orders", "application/json", nil)
}

func helper(t *testing.T) {}
"#;
    let facts = test_facts(&parse_go_source(source));

    assert_eq!(
        facts,
        [
            "Consumer Httptest TestCreateOrder: POST /orders",
            "Consumer Httptest TestGetOrder: GET /orders/42",
            "Test GoTest TestCreateOrder: ",
            "Test GoTest TestGetOrder: ",
        ]
    );
}

#[test]
fn java_tests_should_recognize_mockmvc_restassured_and_rest_templates() {
    let source = r#"
import org.junit.jupiter.api.Test;
import org.springframework.test.web.servlet.MockMvc;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;
import static io.restassured.RestAssured.given;

class OrdersTest {
    @Autowired
    private MockMvc mockMvc;
    @Autowired
    private TestRestTemplate rest;

    @Test
    void readsOrder() throws Exception {
        mockMvc.perform(get("/orders/{id}", 42)).andExpect(status().isOk());
    }

    @ParameterizedTest
    @ValueSource(strings = {"A", "B"})
    void createsOrder(String sku) {
        given().body(sku).when().post("/orders").then().statusCode(201);
    }

    @Test
    public void deletesOrder() {
        rest.exchange("/orders/7", HttpMethod.DELETE, null, Void.class);
    }

    private void helper() {}
}
"#;
    let facts = test_facts(&parse_java_source(source));

    assert_eq!(
        facts,
        [
            "Consumer MockMvc readsOrder: GET /orders/{id}",
            "Consumer RestAssured createsOrder: POST /orders",
            "Consumer TestRestTemplate deletesOrder: DELETE /orders/7",
            "Test JUnit createsOrder: ",
            "Test JUnit deletesOrder: ",
            "Test JUnit readsOrder: ",
        ]
    );
}
