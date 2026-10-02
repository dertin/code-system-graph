//! Repository-level composition of HTTP client calls through wrappers, helpers, and fixtures.
//!
//! Extractors record client calls whose URL depends on parameters of the enclosing function, and
//! the calls between functions with their string arguments. Composition first instantiates those
//! wrapper URLs at every call site that binds them, also through wrappers of wrappers. It then
//! attributes to each test every client call reached through the helpers and fixtures it calls,
//! so that a test is linked to the endpoints it exercises indirectly.
//!
//! A client call issued through a parameter of its function, such as a pytest `client` fixture,
//! is confirmed only when the parameter is a fixture returning an in-process test client, or is
//! passed such a fixture by every resolved caller path that reaches it.

use std::collections::{BTreeMap, BTreeSet};

use crate::repository_symbols::{
    RepositorySourceFile, RepositorySymbols, SymbolFile, SymbolKey, files_by_repository
};
use crate::source_http::instantiated_consumer;
use crate::url_template::BoundArguments;
use crate::{
    SourceEpistemicStatus, SourceFramework, SourceObservation, SourceRole, SymbolRef, UrlPart, UrlTemplate
};

/// Rounds of wrapper instantiation, which is also the longest chain of wrappers that pass a URL
/// parameter through to another wrapper.
const MAX_WRAPPER_HOPS: usize = 3;
/// Longest call chain followed from a test to the client calls of its helpers.
const MAX_TEST_CALL_DEPTH: usize = 3;
/// Most unresolved wrapper URLs retained for one function.
const MAX_OPEN_URLS: usize = 16;

/// A named function of one file, identified by the file's index within its repository.
type Function<'a> = (usize, &'a str);

/// Composes client calls through wrappers, helpers, and fixtures.
///
/// The result has one observation list per input file, in input order. Call observations are
/// consumed; each function that binds a wrapper URL to an exact path gains a consumer
/// observation, and each test gains the client calls of the helpers and fixtures it reaches.
#[must_use]
pub fn compose_client_flows(files: &[RepositorySourceFile<'_>]) -> Vec<Vec<SourceObservation>> {
    let mut output = files
        .iter()
        .map(|file| file.observations.to_vec())
        .collect::<Vec<_>>();
    for members in files_by_repository(files).into_values() {
        let composable = members.iter().any(|&index| {
            files[index]
                .observations
                .iter()
                .any(|observation| observation.role == SourceRole::Call || is_received(observation))
        });
        if !composable {
            continue;
        }
        let flows = RepositoryFlows::new(files, &members);
        for (local, additions) in flows.compose() {
            output[members[local]].extend(additions);
        }
    }
    for observations in &mut output {
        observations.retain(|observation| {
            !matches!(observation.role, SourceRole::Call | SourceRole::Client)
                && !is_received(observation)
        });
    }
    output
}

/// Whether `observation` is a client call issued through a parameter of its function.
fn is_received(observation: &SourceObservation) -> bool {
    observation.role == SourceRole::Consumer
        && matches!(observation.router, Some(SymbolRef::Parameter { .. }))
}

/// A client call whose URL still depends on parameters of the function issuing it.
#[derive(Clone)]
struct OpenUrl<'a> {
    origin: &'a SourceObservation,
    url: UrlTemplate,
}

struct RepositoryFlows<'a> {
    symbols: RepositorySymbols<'a>,
    functions: BTreeMap<Vec<String>, Option<Function<'a>>>,
    locals: BTreeMap<(usize, String), Function<'a>>,
    consumers: BTreeMap<Function<'a>, Vec<&'a SourceObservation>>,
    calls: BTreeMap<Function<'a>, Vec<&'a SourceObservation>>,
    callers: BTreeMap<Function<'a>, Vec<(Function<'a>, &'a SourceObservation)>>,
    tests: BTreeSet<Function<'a>>,
    clients: BTreeMap<(usize, String), SourceFramework>,
    received: BTreeMap<Function<'a>, Vec<SourceObservation>>,
}

impl<'a> RepositoryFlows<'a> {
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
        let mut consumers = BTreeMap::<Function<'a>, Vec<&'a SourceObservation>>::new();
        let mut calls = BTreeMap::<Function<'a>, Vec<&'a SourceObservation>>::new();
        let mut tests = BTreeSet::new();
        let mut clients = BTreeMap::new();
        for (local, &index) in members.iter().enumerate() {
            for observation in files[index].observations {
                let Some(name) = observation.symbol_name.as_deref() else {
                    continue;
                };
                match observation.role {
                    SourceRole::Consumer => consumers
                        .entry((local, name))
                        .or_default()
                        .push(observation),
                    SourceRole::Call => calls.entry((local, name)).or_default().push(observation),
                    SourceRole::Test => {
                        tests.insert((local, name));
                    }
                    SourceRole::Client => {
                        clients.insert((local, name.to_owned()), observation.framework);
                    }
                    SourceRole::Provider | SourceRole::Factory | SourceRole::Mount => {}
                }
            }
        }
        let mut functions = BTreeMap::<Vec<String>, Option<Function<'a>>>::new();
        let mut locals = BTreeMap::new();
        for &function in consumers.keys().chain(calls.keys()) {
            locals.insert((function.0, function.1.to_owned()), function);
            let key = symbols.declare_function(function.0, function.1);
            functions
                .entry(key)
                .and_modify(|declared| {
                    if *declared != Some(function) {
                        *declared = None;
                    }
                })
                .or_insert(Some(function));
        }
        let mut flows = Self {
            symbols,
            functions,
            locals,
            consumers,
            calls,
            callers: BTreeMap::new(),
            tests,
            clients,
            received: BTreeMap::new(),
        };
        for (&caller, calls) in &flows.calls {
            for &call in calls {
                if let Some(callee) = flows.callee(caller.0, call) {
                    flows
                        .callers
                        .entry(callee)
                        .or_default()
                        .push((caller, call));
                }
            }
        }
        flows.received = flows.resolve_received();
        flows
    }

    /// Client calls issued through a parameter that receives an in-process test client.
    fn resolve_received(&self) -> BTreeMap<Function<'a>, Vec<SourceObservation>> {
        let mut received = BTreeMap::<Function<'a>, Vec<SourceObservation>>::new();
        for (&function, consumers) in &self.consumers {
            for &consumer in consumers {
                let Some(SymbolRef::Parameter { name, index }) = &consumer.router else {
                    continue;
                };
                let Some(framework) = self.received_client(function, name, *index, 0) else {
                    continue;
                };
                let mut confirmed = consumer.clone();
                confirmed.framework = framework;
                confirmed.router = None;
                if confirmed.method.is_some() && confirmed.path.is_some() {
                    confirmed.status = SourceEpistemicStatus::Confirmed;
                    confirmed.confidence = 1.0;
                }
                received.entry(function).or_default().push(confirmed);
            }
        }
        received
    }

    /// Test client framework passed to parameter `name` at position `index` of `function`.
    fn received_client(
        &self,
        function: Function<'a>,
        name: &str,
        index: usize,
        depth: usize,
    ) -> Option<SourceFramework> {
        let fixture = SymbolRef::Fixture(name.to_owned());
        let requests_fixture = self.calls.get(&function).is_some_and(|calls| {
            calls.iter().any(|call| {
                call.call
                    .as_ref()
                    .is_some_and(|call| call.callee == fixture)
            })
        });
        if requests_fixture {
            let Some(SymbolKey::Local { file, name }) = self.symbols.resolve(function.0, &fixture)
            else {
                return None;
            };
            return self.clients.get(&(file, name)).copied();
        }
        if depth >= MAX_TEST_CALL_DEPTH {
            return None;
        }
        let mut frameworks = self.callers.get(&function)?.iter().map(|(caller, call)| {
            let arguments = bound_arguments(call);
            let argument = arguments
                .by_name
                .get(name)
                .or_else(|| arguments.by_index.get(&index))?;
            match argument.parts.as_slice() {
                [UrlPart::Parameter { name, index }] => {
                    self.received_client(*caller, name, *index, depth + 1)
                }
                _ => None,
            }
        });
        let first = frameworks.next()??;
        frameworks
            .all(|framework| framework == Some(first))
            .then_some(first)
    }

    /// Function called by `call` from file `local`, when it is known in this repository.
    fn callee(&self, local: usize, call: &SourceObservation) -> Option<Function<'a>> {
        let reference = &call.call.as_ref()?.callee;
        match self.symbols.resolve(local, reference)? {
            SymbolKey::Local { file, name } => self.locals.get(&(file, name)).copied(),
            SymbolKey::Function(key) => self.functions.get(&key).copied().flatten(),
        }
    }

    /// Observations added to each file, keyed by the file's index within the repository.
    fn compose(&self) -> BTreeMap<usize, Vec<SourceObservation>> {
        let resolved = self.instantiate_wrappers();
        let mut additions = BTreeMap::<usize, Vec<SourceObservation>>::new();
        for (function, observations) in self.received.iter().chain(&resolved) {
            additions
                .entry(function.0)
                .or_default()
                .extend(observations.iter().cloned());
        }
        for &test in &self.tests {
            let mut seen = self
                .exact_consumers(test, &resolved)
                .into_iter()
                .map(consumer_identity)
                .collect::<BTreeSet<_>>();
            for observation in self.reached_consumers(test, &resolved) {
                if seen.insert(consumer_identity(&observation)) {
                    additions.entry(test.0).or_default().push(observation);
                }
            }
        }
        additions
    }

    /// Consumers obtained by binding wrapper URLs at call sites, keyed by the calling function.
    fn instantiate_wrappers(&self) -> BTreeMap<Function<'a>, Vec<SourceObservation>> {
        let mut open = BTreeMap::<Function<'a>, Vec<OpenUrl<'a>>>::new();
        for (&function, consumers) in &self.consumers {
            for &origin in consumers {
                if let (Some(url), None) = (&origin.url, &origin.path) {
                    open.entry(function).or_default().push(OpenUrl {
                        origin,
                        url: url.clone(),
                    });
                }
            }
        }
        let mut resolved = BTreeMap::<Function<'a>, Vec<SourceObservation>>::new();
        for _ in 0..MAX_WRAPPER_HOPS {
            let snapshot = open.clone();
            let mut changed = false;
            for (&caller, calls) in &self.calls {
                for &call in calls {
                    let Some(callee) = self
                        .callee(caller.0, call)
                        .filter(|callee| *callee != caller)
                    else {
                        continue;
                    };
                    let Some(wrappers) = snapshot.get(&callee) else {
                        continue;
                    };
                    let arguments = bound_arguments(call);
                    for wrapper in wrappers {
                        let url = wrapper.url.bind(&arguments);
                        if let Some(consumer) =
                            instantiated_consumer(wrapper.origin, &url, caller.1, call.lines)
                        {
                            let consumers = resolved.entry(caller).or_default();
                            let identity = consumer_identity(&consumer);
                            if !consumers
                                .iter()
                                .any(|known| consumer_identity(known) == identity)
                            {
                                consumers.push(consumer);
                                changed = true;
                            }
                        } else if url.has_parameters() {
                            let urls = open.entry(caller).or_default();
                            if urls.len() < MAX_OPEN_URLS
                                && !urls.iter().any(|known| known.url == url)
                            {
                                urls.push(OpenUrl {
                                    origin: wrapper.origin,
                                    url,
                                });
                                changed = true;
                            }
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        resolved
    }

    /// Exact consumers issued by `function`, directly or through an instantiated wrapper.
    fn exact_consumers<'r>(
        &'r self,
        function: Function<'a>,
        resolved: &'r BTreeMap<Function<'a>, Vec<SourceObservation>>,
    ) -> Vec<&'r SourceObservation>
    where
        'a: 'r,
    {
        self.consumers
            .get(&function)
            .into_iter()
            .flatten()
            .copied()
            .filter(|observation| {
                observation.status == SourceEpistemicStatus::Confirmed
                    && observation.method.is_some()
                    && observation.path.is_some()
            })
            .chain(self.received.get(&function).into_iter().flatten())
            .chain(resolved.get(&function).into_iter().flatten())
            .collect()
    }

    /// Exact consumers of the functions reached from `test`, attributed to `test` at the call
    /// site in the test through which each one is reached.
    fn reached_consumers(
        &self,
        test: Function<'a>,
        resolved: &BTreeMap<Function<'a>, Vec<SourceObservation>>,
    ) -> Vec<SourceObservation> {
        let mut reached = Vec::new();
        let mut visited = BTreeSet::from([test]);
        let mut frontier = self
            .calls
            .get(&test)
            .into_iter()
            .flatten()
            .filter_map(|call| Some((self.callee(test.0, call)?, call.lines)))
            .collect::<Vec<_>>();
        for _ in 0..MAX_TEST_CALL_DEPTH {
            let mut next = Vec::new();
            for (function, lines) in frontier {
                if !visited.insert(function) {
                    continue;
                }
                for consumer in self.exact_consumers(function, resolved) {
                    let mut attributed = consumer.clone();
                    attributed.symbol_name = Some(test.1.to_owned());
                    attributed.lines = lines;
                    attributed.url = None;
                    reached.push(attributed);
                }
                next.extend(
                    self.calls
                        .get(&function)
                        .into_iter()
                        .flatten()
                        .filter_map(|call| Some((self.callee(function.0, call)?, lines))),
                );
            }
            frontier = next;
        }
        reached
    }
}

/// Arguments of `call` keyed by keyword, or by position among the positional arguments.
fn bound_arguments(call: &SourceObservation) -> BoundArguments {
    let mut bound = BoundArguments::default();
    let mut position = 0;
    for argument in call.call.iter().flat_map(|call| &call.arguments) {
        match (&argument.keyword, &argument.value) {
            (Some(keyword), Some(value)) => {
                bound.by_name.insert(keyword.clone(), value.clone());
            }
            (Some(_), None) => {}
            (None, value) => {
                if let Some(value) = value {
                    bound.by_index.insert(position, value.clone());
                }
                position += 1;
            }
        }
    }
    bound
}

/// Method, path, and authority of a consumer.
type ConsumerIdentity = (Option<String>, Option<String>, Option<String>);

fn consumer_identity(observation: &SourceObservation) -> ConsumerIdentity {
    (
        observation.method.clone(),
        observation.path.clone(),
        observation.authority.clone(),
    )
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::RepoId;

    use super::compose_client_flows;
    use crate::{
        RepositorySourceFile, SourceObservation, SourceRole, parse_python_source, parse_typescript_source
    };

    /// Consumers after composition as `path: symbol METHOD authority path`.
    fn composed_consumers(files: &[(&str, Vec<SourceObservation>)]) -> Vec<String> {
        let repo = RepoId::new("repo:shop");
        let inputs = files
            .iter()
            .map(|(path, observations)| RepositorySourceFile {
                repo_id: &repo,
                path,
                observations,
            })
            .collect::<Vec<_>>();
        let composed = compose_client_flows(&inputs);
        assert!(
            composed
                .iter()
                .flatten()
                .all(|item| item.role != SourceRole::Call)
        );
        files
            .iter()
            .zip(composed)
            .flat_map(|((path, _), observations)| {
                observations
                    .into_iter()
                    .filter(|item| item.role == SourceRole::Consumer && item.path.is_some())
                    .map(move |item| {
                        format!(
                            "{path}: {} {} {} {}",
                            item.symbol_name.unwrap_or_default(),
                            item.method.unwrap_or_default(),
                            item.authority.unwrap_or_default(),
                            item.path.unwrap_or_default()
                        )
                    })
            })
            .collect()
    }

    #[test]
    fn python_tests_should_reach_fixture_and_wrapped_helper_calls_across_files() {
        let conftest = r#"
import pytest
import requests
BASE = "http://orders:8000"

@pytest.fixture
def order():
    return requests.post(BASE + "/orders", json={}).json()
"#;
        let helpers = r#"
import requests
API = "http://orders:8000/api"

def get_json(path):
    return requests.get(API + path).json()

def read_order(order_id):
    return get_json(f"/orders/{order_id}")
"#;
        let test = r#"
from tests.helpers import read_order

def test_read(order):
    read_order(order["id"])
"#;
        let consumers = composed_consumers(&[
            ("tests/conftest.py", parse_python_source(conftest)),
            ("tests/helpers.py", parse_python_source(helpers)),
            ("tests/test_orders.py", parse_python_source(test)),
        ]);

        assert_eq!(
            consumers,
            [
                "tests/conftest.py: order POST orders:8000 /orders",
                "tests/helpers.py: read_order GET orders:8000 /api/orders/{order_id}",
                "tests/test_orders.py: test_read POST orders:8000 /orders",
                "tests/test_orders.py: test_read GET orders:8000 /api/orders/{order_id}",
            ]
        );
    }

    #[test]
    fn python_parameter_clients_should_resolve_through_test_client_fixtures() {
        let conftest = r"
import pytest
from fastapi.testclient import TestClient
from app.main import app

@pytest.fixture
def client():
    with TestClient(app) as test_client:
        yield test_client
";
        let helpers = r#"
def create_order(client, sku):
    return client.post("/orders", json={"sku": sku})

def health(session):
    return session.get("/health")
"#;
        let test = r#"
from tests.helpers import create_order, health

def test_create(client):
    create_order(client, "A-1")

def test_read(client):
    client.get("/orders/42")

def test_health():
    health(object())
"#;
        let files = [
            ("tests/conftest.py", parse_python_source(conftest)),
            ("tests/helpers.py", parse_python_source(helpers)),
            ("tests/test_orders.py", parse_python_source(test)),
        ];
        let consumers = composed_consumers(&files);

        assert_eq!(
            consumers,
            [
                "tests/helpers.py: create_order POST  /orders",
                "tests/test_orders.py: test_read GET  /orders/42",
                "tests/test_orders.py: test_create POST  /orders",
            ]
        );
    }

    #[test]
    fn script_wrappers_should_be_instantiated_at_call_sites_in_other_modules() {
        let api = r#"
const API = "/api";
export function request(path: string) {
  return fetch(API + path);
}
"#;
        let orders = r#"
import { request } from "./api";
export function getOrder(id: string) {
  return request(`/orders/${id}`);
}
"#;
        let consumers = composed_consumers(&[
            ("web/src/api.ts", parse_typescript_source(api)),
            ("web/src/orders.ts", parse_typescript_source(orders)),
        ]);

        assert_eq!(
            consumers,
            ["web/src/orders.ts: getOrder GET  /api/orders/{id}"]
        );
    }
}
