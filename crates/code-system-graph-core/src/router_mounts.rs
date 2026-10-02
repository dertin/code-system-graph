//! Repository-level composition of router prefixes declared across source files.
//!
//! Extractors record the router each provider route is registered on and every mount of one
//! router under a prefix on another. Composition resolves those references per repository, through
//! file-local names, relative and package imports, and function paths, then rewrites each route
//! path with every prefix chain that reaches it.

use std::collections::{BTreeMap, BTreeSet};

use crate::repository_symbols::{
    RepositorySourceFile, RepositorySymbols, SymbolFile, SymbolKey, files_by_repository
};
use crate::{
    SourceEpistemicStatus, SourceLanguage, SourceObservation, SourceRole, SymbolRef, normalize_source_http_path
};

/// Longest mount chain followed from a route to the application root.
const MAX_MOUNT_DEPTH: usize = 8;
/// Most composed paths retained for one router.
const MAX_COMPOSED_PREFIXES: usize = 16;

/// Rewrites provider paths with the router prefixes mounted above them.
///
/// The result has one observation list per input file, in input order. Mount observations are
/// consumed, a route on a router mounted at several places is repeated once per composed path,
/// and routes whose router is not mounted anywhere keep their path.
#[must_use]
pub fn compose_router_mounts(files: &[RepositorySourceFile<'_>]) -> Vec<Vec<SourceObservation>> {
    let by_repository = files_by_repository(files);
    let mut output = files
        .iter()
        .map(|file| file.observations.to_vec())
        .collect::<Vec<_>>();
    for members in by_repository.into_values() {
        let repository = RepositoryRouters::new(files, &members);
        if repository.mounts.is_empty() {
            for &index in &members {
                output[index].retain(|observation| observation.role != SourceRole::Mount);
            }
            continue;
        }
        let mut cache = BTreeMap::new();
        for (local, &index) in members.iter().enumerate() {
            output[index] = repository.rewrite(local, &mut cache);
        }
    }
    output
}

struct RepositoryRouters<'a> {
    symbols: RepositorySymbols<'a>,
    mounts: BTreeMap<SymbolKey, Vec<(Option<SymbolKey>, String)>>,
}

impl<'a> RepositoryRouters<'a> {
    fn new(files: &'a [RepositorySourceFile<'a>], members: &[usize]) -> Self {
        let mut symbols = RepositorySymbols::new(
            members
                .iter()
                .map(|&index| SymbolFile {
                    path: files[index].path,
                    observations: files[index].observations,
                })
                .collect(),
        );
        for local in 0..members.len() {
            for observation in symbols.files()[local].observations {
                for reference in [&observation.router, &observation.mount_parent]
                    .into_iter()
                    .flatten()
                {
                    if let SymbolRef::Function(name) = reference {
                        symbols.declare_function(local, name);
                    }
                }
            }
        }
        let mut mounts = BTreeMap::<SymbolKey, Vec<(Option<SymbolKey>, String)>>::new();
        for local in 0..members.len() {
            for observation in symbols.files()[local]
                .observations
                .iter()
                .filter(|observation| observation.role == SourceRole::Mount)
            {
                let Some(child) = observation
                    .router
                    .as_ref()
                    .and_then(|reference| symbols.resolve(local, reference))
                else {
                    continue;
                };
                let parent = observation
                    .mount_parent
                    .as_ref()
                    .and_then(|reference| symbols.resolve(local, reference));
                if parent.as_ref() == Some(&child) {
                    continue;
                }
                let prefix = observation
                    .path
                    .as_deref()
                    .map(prefix_text)
                    .unwrap_or_default();
                mounts.entry(child).or_default().push((parent, prefix));
            }
        }
        for edges in mounts.values_mut() {
            edges.sort();
            edges.dedup();
        }
        Self { symbols, mounts }
    }

    fn rewrite(
        &self,
        local: usize,
        cache: &mut BTreeMap<SymbolKey, Vec<String>>,
    ) -> Vec<SourceObservation> {
        let observations = self.symbols.files()[local].observations;
        let mut output = Vec::with_capacity(observations.len());
        for observation in observations {
            match observation.role {
                SourceRole::Mount => {}
                SourceRole::Provider => {
                    let prefixes = observation
                        .router
                        .as_ref()
                        .and_then(|reference| self.symbols.resolve(local, reference))
                        .map(|key| self.prefixes(&key, cache))
                        .unwrap_or_default();
                    match (&observation.path, prefixes.as_slice()) {
                        (Some(path), [_, ..]) if prefixes != [""] => {
                            output.extend(prefixes.iter().map(|prefix| {
                                let mut composed = observation.clone();
                                composed.path =
                                    Some(normalize_source_http_path(&format!("{prefix}{path}")));
                                composed
                            }));
                        }
                        _ => output.push(observation.clone()),
                    }
                }
                SourceRole::Consumer
                | SourceRole::Test
                | SourceRole::Factory
                | SourceRole::Call
                | SourceRole::Client => {
                    output.push(observation.clone());
                }
            }
        }
        output
    }

    /// Returns every prefix chain from `key` to a root, outermost prefix first.
    fn prefixes(
        &self,
        key: &SymbolKey,
        cache: &mut BTreeMap<SymbolKey, Vec<String>>,
    ) -> Vec<String> {
        if let Some(cached) = cache.get(key) {
            return cached.clone();
        }
        let mut visiting = Vec::new();
        let mut prefixes = self.walk(key, &mut visiting);
        if prefixes.is_empty() {
            prefixes.push(String::new());
        }
        cache.insert(key.clone(), prefixes.clone());
        prefixes
    }

    /// Chains through a cycle or deeper than [`MAX_MOUNT_DEPTH`] are dropped.
    fn walk(&self, key: &SymbolKey, visiting: &mut Vec<SymbolKey>) -> Vec<String> {
        let Some(edges) = self.mounts.get(key) else {
            return vec![String::new()];
        };
        if visiting.len() >= MAX_MOUNT_DEPTH || visiting.contains(key) {
            return Vec::new();
        }
        visiting.push(key.clone());
        let mut prefixes = BTreeSet::new();
        for (parent, prefix) in edges {
            let outer = parent
                .as_ref()
                .map_or_else(|| vec![String::new()], |parent| self.walk(parent, visiting));
            prefixes.extend(outer.into_iter().map(|outer| format!("{outer}{prefix}")));
        }
        visiting.pop();
        prefixes.into_iter().take(MAX_COMPOSED_PREFIXES).collect()
    }
}

/// Normalized prefix text without a trailing slash; the root prefix is empty.
fn prefix_text(path: &str) -> String {
    let normalized = normalize_source_http_path(path);
    if normalized == "/" {
        String::new()
    } else {
        normalized
    }
}

/// Builds a mount observation recorded by a source extractor.
#[must_use]
pub(crate) fn mount_observation(
    language: SourceLanguage,
    framework: crate::SourceFramework,
    child: SymbolRef,
    parent: Option<SymbolRef>,
    prefix: Option<&str>,
    lines: crate::SourceLineRange,
) -> SourceObservation {
    SourceObservation {
        language,
        framework,
        role: SourceRole::Mount,
        method: None,
        path: Some(normalize_source_http_path(prefix.unwrap_or("/"))),
        symbol_name: None,
        related_symbol: None,
        related_path: None,
        authority: None,
        router: Some(child),
        mount_parent: parent,
        url: None,
        call: None,
        lines,
        status: SourceEpistemicStatus::Confirmed,
        confidence: 1.0,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::RepoId;

    use super::{compose_router_mounts, mount_observation};
    use crate::{
        RepositorySourceFile, SourceEpistemicStatus, SourceFramework, SourceLanguage, SourceLineRange, SourceObservation, SourceRole, SymbolRef
    };

    const LINES: SourceLineRange = SourceLineRange { start: 1, end: 1 };

    fn route(language: SourceLanguage, path: &str, router: SymbolRef) -> SourceObservation {
        SourceObservation {
            language,
            framework: SourceFramework::Express,
            role: SourceRole::Provider,
            method: Some("GET".to_owned()),
            path: Some(path.to_owned()),
            symbol_name: Some("handler".to_owned()),
            related_symbol: None,
            related_path: None,
            authority: None,
            router: Some(router),
            mount_parent: None,
            url: None,
            call: None,
            lines: LINES,
            status: SourceEpistemicStatus::Confirmed,
            confidence: 1.0,
            warnings: Vec::new(),
        }
    }

    fn mount(
        language: SourceLanguage,
        child: SymbolRef,
        parent: Option<SymbolRef>,
        prefix: &str,
    ) -> SourceObservation {
        mount_observation(
            language,
            SourceFramework::Express,
            child,
            parent,
            Some(prefix),
            LINES,
        )
    }

    fn composed_paths(files: &[(&str, Vec<SourceObservation>)]) -> Vec<String> {
        let repo = RepoId::new("repo:api");
        let inputs = files
            .iter()
            .map(|(path, observations)| RepositorySourceFile {
                repo_id: &repo,
                path,
                observations,
            })
            .collect::<Vec<_>>();
        let mut paths = compose_router_mounts(&inputs)
            .into_iter()
            .flatten()
            .filter_map(|observation| observation.path)
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn local(name: &str) -> SymbolRef {
        SymbolRef::Local(name.to_owned())
    }

    #[test]
    fn script_imports_and_default_exports_should_compose_nested_prefixes() {
        let js = SourceLanguage::TypeScript;
        let files = [
            (
                "src/routes/orders.ts",
                vec![
                    route(js, "/:id", local("router")),
                    mount(js, local("router"), Some(local("default")), "/"),
                ],
            ),
            (
                "src/routes/index.ts",
                vec![mount(
                    js,
                    SymbolRef::Import {
                        module: "./orders.js".to_owned(),
                        name: "default".to_owned(),
                    },
                    Some(local("api")),
                    "/orders",
                )],
            ),
            (
                "src/app.ts",
                vec![
                    mount(
                        js,
                        SymbolRef::Import {
                            module: "./routes".to_owned(),
                            name: "api".to_owned(),
                        },
                        Some(local("app")),
                        "/v1",
                    ),
                    mount(
                        js,
                        SymbolRef::Import {
                            module: "./routes/index".to_owned(),
                            name: "api".to_owned(),
                        },
                        Some(local("app")),
                        "/v2/",
                    ),
                ],
            ),
        ];

        assert_eq!(
            composed_paths(&files),
            ["/v1/orders/:id".to_owned(), "/v2/orders/:id".to_owned()]
        );
    }

    #[test]
    fn python_modules_should_resolve_absolute_and_relative_imports() {
        let py = SourceLanguage::Python;
        let files = [
            (
                "src/app/api/users.py",
                vec![route(py, "/users/{id}", local("router"))],
            ),
            (
                "src/app/api/__init__.py",
                vec![mount(
                    py,
                    SymbolRef::Import {
                        module: ".users".to_owned(),
                        name: "router".to_owned(),
                    },
                    Some(local("api")),
                    "/internal",
                )],
            ),
            (
                "src/app/main.py",
                vec![mount(
                    py,
                    SymbolRef::Import {
                        module: "app".to_owned(),
                        name: "api.api".to_owned(),
                    },
                    Some(local("app")),
                    "/v1",
                )],
            ),
        ];

        assert_eq!(
            composed_paths(&files),
            ["/v1/internal/users/{id}".to_owned()]
        );
    }

    #[test]
    fn function_routers_should_resolve_by_module_path() {
        let rust = SourceLanguage::Rust;
        let files = [
            (
                "crates/api/src/orders/mod.rs",
                vec![route(
                    rust,
                    "/{id}",
                    SymbolRef::Function("routes".to_owned()),
                )],
            ),
            (
                "crates/api/src/users.rs",
                vec![route(
                    rust,
                    "/{id}",
                    SymbolRef::Function("routes".to_owned()),
                )],
            ),
            (
                "crates/api/src/main.rs",
                vec![
                    mount(
                        rust,
                        SymbolRef::Call("crate::orders::routes".to_owned()),
                        Some(SymbolRef::Function("app".to_owned())),
                        "/orders",
                    ),
                    mount(
                        rust,
                        SymbolRef::Call("routes".to_owned()),
                        Some(SymbolRef::Function("app".to_owned())),
                        "/ambiguous",
                    ),
                ],
            ),
        ];

        assert_eq!(
            composed_paths(&files),
            ["/orders/{id}".to_owned(), "/{id}".to_owned()]
        );
    }

    fn extracted_routes(files: &[(&str, &str)]) -> Vec<String> {
        let repo = RepoId::new("repo:api");
        let observations = files
            .iter()
            .map(|(path, source)| {
                let extension = path.rsplit_once('.').map(|(_, extension)| extension);
                match extension {
                    Some("py") => crate::parse_python_source(source),
                    Some("rs") => crate::parse_rust_source(source),
                    Some("go") => crate::parse_go_source(source),
                    Some("java") => crate::parse_java_source(source),
                    Some("js") => crate::parse_javascript_source_at_path(path, source),
                    _ => crate::parse_typescript_source_at_path(path, source),
                }
            })
            .collect::<Vec<_>>();
        let inputs = files
            .iter()
            .zip(&observations)
            .map(|((path, _), observations)| RepositorySourceFile {
                repo_id: &repo,
                path,
                observations,
            })
            .collect::<Vec<_>>();
        let mut routes = compose_router_mounts(&inputs)
            .into_iter()
            .flatten()
            .filter(|observation| {
                observation.role == SourceRole::Provider
                    && observation.status == SourceEpistemicStatus::Confirmed
            })
            .filter_map(|observation| {
                Some(format!(
                    "{} {} {}",
                    observation.method?, observation.path?, observation.symbol_name?
                ))
            })
            .collect::<Vec<_>>();
        routes.sort();
        routes
    }

    #[test]
    fn fastapi_and_flask_mounts_should_compose_across_modules() {
        let routes = extracted_routes(&[
            (
                "app/api/users.py",
                "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/users\")\n\n@router.get(\"/{user_id}\")\ndef read_user(user_id: int):\n    return {}\n",
            ),
            (
                "app/main.py",
                "from fastapi import FastAPI\nfrom app.api import users\nfrom app.api.users import router as users_router\napp = FastAPI()\napp.include_router(users.router, prefix=\"/v1\")\napp.include_router(users_router, prefix=\"/v2\")\n",
            ),
            (
                "shop/views.py",
                "from flask import Blueprint\nbp = Blueprint(\"orders\", __name__, url_prefix=\"/orders\")\n\n@bp.route(\"/<int:order_id>\", methods=[\"GET\"])\ndef show(order_id):\n    return ''\n",
            ),
            (
                "shop/__init__.py",
                "from flask import Flask\nfrom .views import bp\napp = Flask(__name__)\napp.register_blueprint(bp, url_prefix=\"/shop\")\n",
            ),
        ]);

        assert_eq!(
            routes,
            [
                "GET /shop/orders/<int:order_id> show",
                "GET /v1/users/{user_id} read_user",
                "GET /v2/users/{user_id} read_user",
            ]
        );
    }

    #[test]
    fn express_and_nest_mounts_should_compose_across_modules() {
        let routes = extracted_routes(&[
            (
                "src/routes/orders.ts",
                "import { Router } from 'express';\nconst router = Router();\nrouter.get('/:id', getOrder);\nexport default router;\n",
            ),
            (
                "src/routes/users.js",
                "const express = require('express');\nfunction buildUsers() {\n  const users = express.Router();\n  users.get('/:id', getUser);\n  return users;\n}\nmodule.exports = { buildUsers };\n",
            ),
            (
                "src/app.ts",
                "import express from 'express';\nimport orders from './routes/orders';\nconst { buildUsers } = require('./routes/users');\nconst app = express();\nconst api = express.Router();\napi.use('/orders', orders);\napi.use('/users', authenticate, buildUsers());\napp.use('/api', api);\n",
            ),
            (
                "src/catalog/catalog.controller.ts",
                "import { Controller, Get } from '@nestjs/common';\n@Controller('catalog')\nexport class CatalogController {\n  @Get(':sku')\n  findOne() {}\n  @Get()\n  findAll() {}\n}\n",
            ),
            (
                "src/main.ts",
                "import { NestFactory } from '@nestjs/core';\nasync function bootstrap() {\n  const app = await NestFactory.create(AppModule);\n  app.setGlobalPrefix('v2');\n}\n",
            ),
        ]);

        assert_eq!(
            routes,
            [
                "GET /api/orders/:id getOrder",
                "GET /api/users/:id getUser",
                "GET /v2/catalog findAll",
                "GET /v2/catalog/:sku findOne",
            ]
        );
    }

    #[test]
    fn spring_class_mappings_should_prefix_method_mappings() {
        let routes = extracted_routes(&[(
            "src/main/java/shop/OrderController.java",
            "import org.springframework.web.bind.annotation.*;\n@RestController\n@RequestMapping(\"/api/orders\")\npublic class OrderController {\n  @GetMapping(\"/{id}\")\n  public Order get(@PathVariable long id) { return null; }\n  @PostMapping\n  public Order create() { return null; }\n}\n",
        )]);

        assert_eq!(
            routes,
            ["GET /api/orders/{id} get", "POST /api/orders create"]
        );
    }

    #[test]
    fn gin_chi_and_net_http_mounts_should_compose_across_packages() {
        let routes = extracted_routes(&[
            (
                "internal/routes/orders.go",
                "package routes\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc RegisterOrders(rg *gin.RouterGroup) {\n\trg.GET(\"/:id\", getOrder)\n}\n",
            ),
            (
                "cmd/api/main.go",
                "package main\n\nimport (\n\t\"github.com/gin-gonic/gin\"\n\t\"example.com/internal/routes\"\n)\n\nfunc main() {\n\tr := gin.Default()\n\tv1 := r.Group(\"/v1\")\n\t{\n\t\tv1.GET(\"/health\", health)\n\t}\n\troutes.RegisterOrders(v1.Group(\"/orders\"))\n}\n",
            ),
            (
                "billing/server.go",
                "package billing\n\nimport (\n\t\"net/http\"\n\t\"github.com/go-chi/chi/v5\"\n)\n\nfunc invoices() http.Handler {\n\tr := chi.NewRouter()\n\tr.Get(\"/{id}\", getInvoice)\n\treturn r\n}\n\nfunc Router() http.Handler {\n\tr := chi.NewRouter()\n\tr.Route(\"/billing\", func(r chi.Router) {\n\t\tr.Get(\"/status\", status)\n\t\tr.Mount(\"/invoices\", invoices())\n\t})\n\tmux := http.NewServeMux()\n\tmux.HandleFunc(\"GET /payments/{id}\", getPayment)\n\treturn r\n}\n",
            ),
        ]);

        assert_eq!(
            routes,
            [
                "GET /billing/invoices/{id} getInvoice",
                "GET /billing/status status",
                "GET /payments/{id} getPayment",
                "GET /v1/health health",
                "GET /v1/orders/:id getOrder",
            ]
        );
    }

    #[test]
    fn axum_and_actix_mounts_should_compose_across_modules() {
        let routes = extracted_routes(&[
            (
                "orders/src/orders.rs",
                "use axum::{routing::get, Router};\npub fn routes() -> Router {\n    Router::new().route(\"/{id}\", get(get_order))\n}\n",
            ),
            (
                "orders/src/main.rs",
                "use axum::{routing::get, Router};\nfn app() -> Router {\n    let api = Router::new().route(\"/health\", get(health));\n    Router::new()\n        .nest(\"/orders\", crate::orders::routes())\n        .nest(\"/api\", api)\n}\n",
            ),
            (
                "billing/src/handlers.rs",
                "use actix_web::{get, web, HttpResponse};\n#[get(\"/invoices/{id}\")]\nasync fn get_invoice() -> HttpResponse { HttpResponse::Ok().finish() }\npub fn config(cfg: &mut web::ServiceConfig) {\n    cfg.service(web::scope(\"/admin\").route(\"/audit\", web::get().to(audit)));\n}\n",
            ),
            (
                "billing/src/main.rs",
                "use actix_web::{web, App, HttpServer};\nasync fn main() {\n    HttpServer::new(|| {\n        App::new()\n            .service(web::scope(\"/api\").service(handlers::get_invoice))\n            .configure(handlers::config)\n    });\n}\n",
            ),
        ]);

        assert_eq!(
            routes,
            [
                "GET /admin/audit audit",
                "GET /api/health health",
                "GET /api/invoices/{id} get_invoice",
                "GET /orders/{id} get_order",
            ]
        );
    }

    #[test]
    fn mount_cycles_and_unresolved_routers_should_keep_route_paths() {
        let go = SourceLanguage::Go;
        let files = [(
            "internal/http/routes.go",
            vec![
                route(go, "/orders", local("a")),
                route(go, "/users", local("lonely")),
                mount(go, local("a"), Some(local("b")), "/a"),
                mount(go, local("b"), Some(local("a")), "/b"),
                mount(
                    go,
                    SymbolRef::Call("missing.Register".to_owned()),
                    None,
                    "/x",
                ),
            ],
        )];

        assert_eq!(
            composed_paths(&files),
            ["/orders".to_owned(), "/users".to_owned()]
        );
    }
}
