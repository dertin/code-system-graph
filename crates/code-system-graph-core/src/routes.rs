//! Canonical HTTP route shapes and the single provider index shared by every HTTP link kind.
//!
//! Providers are indexed by method and canonical shape. Consumers, tests, and implementation
//! anchors resolve through the same index, so a concrete request path such as `/orders/42` reaches
//! the `/orders/{id}` template regardless of the framework syntax that declared it.

use std::collections::BTreeMap;

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, HttpLinkGap, HttpLinkGapReason, HttpLinkReport, Node, NodeId, RepoId, stable_id
};

use crate::linker::{http_link_ambiguity, sort_http_ambiguities};
use crate::{
    BoundaryRole, DeclaredImplementation, DeclaredTestCase, HttpBoundary, HttpLinkAmbiguity, HttpLinkResolution, InfrastructureDocument, InfrastructureResourceKind
};

/// One segment of a canonical route template.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RouteSegment {
    /// Literal path segment.
    Static(String),
    /// Single-segment parameter such as `{id}`, `:id`, `<int:id>`, `{id:int}`, or `[id]`.
    Param,
    /// Parameter that consumes one or more remaining segments, such as `{*rest}`, `{path...}`,
    /// `<path:rest>`, `[...slug]`, or `*filepath`.
    CatchAll,
}

/// Framework-independent route template. Parameter names are not part of the shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteShape {
    segments: Vec<RouteSegment>,
}

impl RouteShape {
    /// Parses a normalized path or route template. Query strings and fragments are ignored.
    #[must_use]
    pub fn parse(path: &str) -> Self {
        let path = path.split(['?', '#']).next().unwrap_or_default();
        let mut segments = Vec::new();
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            let classified = classify_segment(segment);
            let terminal = classified == RouteSegment::CatchAll;
            segments.push(classified);
            if terminal {
                break;
            }
        }
        Self { segments }
    }

    /// Returns the canonical segments in path order.
    #[must_use]
    pub fn segments(&self) -> &[RouteSegment] {
        &self.segments
    }

    /// Returns `true` when the shape has no parameter segment.
    #[must_use]
    pub fn is_concrete(&self) -> bool {
        self.segments
            .iter()
            .all(|segment| matches!(segment, RouteSegment::Static(_)))
    }

    /// Renders the identity form: parameters become `{}` and catch-alls become `{*}`.
    #[must_use]
    pub fn canonical(&self) -> String {
        if self.segments.is_empty() {
            return "/".to_owned();
        }
        let mut rendered = String::new();
        for segment in &self.segments {
            rendered.push('/');
            match segment {
                RouteSegment::Static(value) => rendered.push_str(value),
                RouteSegment::Param => rendered.push_str("{}"),
                RouteSegment::CatchAll => rendered.push_str("{*}"),
            }
        }
        rendered
    }
}

/// Returns the canonical identity form of a path or route template.
#[must_use]
pub fn canonical_route(path: &str) -> String {
    RouteShape::parse(path).canonical()
}

fn classify_segment(segment: &str) -> RouteSegment {
    if segment == "*" || segment == "**" {
        return RouteSegment::CatchAll;
    }
    if let Some(inner) = segment
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .filter(|inner| !inner.contains(['{', '}']))
    {
        return if inner.starts_with('*')
            || inner.ends_with("...")
            || inner.ends_with(":path")
            || inner.ends_with(":.*")
            || inner.ends_with(":.+")
        {
            RouteSegment::CatchAll
        } else {
            RouteSegment::Param
        };
    }
    if (segment.starts_with("[...") && segment.ends_with(']'))
        || (segment.starts_with("[[...") && segment.ends_with("]]"))
    {
        return RouteSegment::CatchAll;
    }
    if segment.len() > 2 && segment.starts_with('[') && segment.ends_with(']') {
        return RouteSegment::Param;
    }
    if let Some(inner) = segment
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    {
        return if inner.starts_with("path:") {
            RouteSegment::CatchAll
        } else {
            RouteSegment::Param
        };
    }
    if let Some(name) = segment.strip_prefix(':').filter(|name| !name.is_empty()) {
        return if name.ends_with('*') || name.ends_with('+') {
            RouteSegment::CatchAll
        } else {
            RouteSegment::Param
        };
    }
    if segment.len() > 1 && segment.starts_with('*') {
        return RouteSegment::CatchAll;
    }
    if segment.contains('{') && segment.contains('}') {
        return RouteSegment::Param;
    }
    RouteSegment::Static(segment.to_owned())
}

/// Returns the normalized authority of an absolute `http`/`https` URL.
pub(crate) fn url_authority(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    normalize_authority(authority)
}

/// Normalizes a `host[:port]` authority: lower-case, without user information or a trailing
/// root dot. Returns `None` for an empty host or a value containing a scheme, path, or whitespace.
#[must_use]
pub fn normalize_authority(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(['/', '?', '#', ' ', '\t']) || value.contains("://") {
        return None;
    }
    let host_and_port = value.rsplit_once('@').map_or(value, |(_, host)| host);
    let (host, port) = split_port(host_and_port);
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || port.is_some_and(|port| port.parse::<u16>().is_err()) {
        return None;
    }
    Some(match port {
        Some(port) if host.contains(':') => format!("[{host}]:{port}"),
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn split_port(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => (authority, None),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(port)),
        _ => (authority, None),
    }
}

fn authority_host(authority: &str) -> &str {
    split_port(authority).0
}

/// Returns `true` for loopback and wildcard hosts used by locally started services and tests.
fn is_loopback(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "::1" | "0.0.0.0" | "host.docker.internal"
    ) || host.ends_with(".localhost")
        || host.starts_with("127.")
}

/// Reduces cluster-internal service DNS names such as `orders.shop.svc.cluster.local` to the
/// service name.
fn service_host(host: &str) -> &str {
    let trimmed = host
        .strip_suffix(".svc.cluster.local")
        .or_else(|| host.strip_suffix(".svc"));
    trimmed.map_or(host, |name| name.split('.').next().unwrap_or(name))
}

/// Repository that serves each known authority.
///
/// Declared authorities come from the manifest and strictly restrict resolution to their
/// repository. Inferred authorities come from deployment declarations; when the inferred repository
/// has no matching route, resolution falls back to workspace rules. A host inferred for several
/// repositories is not used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorityMap {
    declared: BTreeMap<String, RepoId>,
    inferred: BTreeMap<String, Option<RepoId>>,
}

/// Resolution target of one call authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorityTarget<'a> {
    Workspace,
    Declared(&'a RepoId),
    Inferred(&'a RepoId),
    External,
}

impl AuthorityMap {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares the repository serving a normalized `host` or `host:port` authority.
    pub fn declare(&mut self, authority: String, repository: RepoId) {
        self.declared.insert(authority, repository);
    }

    /// Records a host inferred from a deployment declaration in `repository`.
    pub fn infer(&mut self, host: &str, repository: &RepoId) {
        let Some(host) = normalize_authority(host) else {
            return;
        };
        let host = service_host(authority_host(&host)).to_owned();
        self.inferred
            .entry(host)
            .and_modify(|existing| {
                if existing.as_ref() != Some(repository) {
                    *existing = None;
                }
            })
            .or_insert_with(|| Some(repository.clone()));
    }

    /// Records every service, deployment, and host alias declared by an infrastructure document.
    pub fn infer_from_infrastructure(
        &mut self,
        repository: &RepoId,
        document: &InfrastructureDocument,
    ) {
        for unit in &document.deployment_units {
            for host in std::iter::once(&unit.name)
                .chain(&unit.service_names)
                .chain(&unit.host_aliases)
            {
                self.infer(host, repository);
            }
        }
        for resource in document
            .resources
            .iter()
            .filter(|resource| resource.kind == InfrastructureResourceKind::Service)
        {
            self.infer(&resource.name, repository);
        }
    }

    fn lookup(&self, authority: &str) -> AuthorityTarget<'_> {
        let host = authority_host(authority);
        if is_loopback(host) {
            return AuthorityTarget::Workspace;
        }
        if let Some(repository) = self
            .declared
            .get(authority)
            .or_else(|| self.declared.get(host))
        {
            return AuthorityTarget::Declared(repository);
        }
        match self.inferred.get(service_host(host)) {
            Some(Some(repository)) => AuthorityTarget::Inferred(repository),
            Some(None) => AuthorityTarget::Workspace,
            None => AuthorityTarget::External,
        }
    }
}

/// How a caller constrains which repositories may provide the operation it invokes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CallScope {
    /// Any repository may provide the operation.
    #[default]
    Workspace,
    /// The call runs in-process against the application in its own repository, such as a
    /// `TestClient(app)`, `httptest`, or `MockMvc` request.
    InProcess,
    /// The call targets a normalized `host:port` authority.
    Authority(String),
    /// The call targets a host outside the workspace and is never linked.
    External,
}

/// Providers sharing one canonical shape, de-duplicated by node identity.
#[derive(Debug, Default)]
struct ProviderSet<'a> {
    providers: Vec<&'a HttpBoundary>,
}

impl<'a> ProviderSet<'a> {
    fn insert(&mut self, provider: &'a HttpBoundary) {
        if let Some(existing) = self
            .providers
            .iter_mut()
            .find(|candidate| candidate.node.id == provider.node.id)
        {
            if provider.evidence.confidence > existing.evidence.confidence {
                *existing = provider;
            }
        } else {
            self.providers.push(provider);
        }
    }

    fn accepted(&self, accept: &impl Fn(&HttpBoundary) -> bool) -> Vec<&'a HttpBoundary> {
        self.providers
            .iter()
            .copied()
            .filter(|provider| accept(provider))
            .collect()
    }
}

#[derive(Debug, Default)]
struct RouteNode<'a> {
    statics: BTreeMap<String, RouteNode<'a>>,
    param: Option<Box<RouteNode<'a>>>,
    catch_all: ProviderSet<'a>,
    terminal: ProviderSet<'a>,
}

impl<'a> RouteNode<'a> {
    fn insert(&mut self, segments: &[RouteSegment], provider: &'a HttpBoundary) {
        let Some((first, rest)) = segments.split_first() else {
            self.terminal.insert(provider);
            return;
        };
        match first {
            RouteSegment::Static(value) => self
                .statics
                .entry(value.clone())
                .or_default()
                .insert(rest, provider),
            RouteSegment::Param => self
                .param
                .get_or_insert_with(Box::default)
                .insert(rest, provider),
            RouteSegment::CatchAll => self.catch_all.insert(provider),
        }
    }

    /// Finds the most specific accepted providers: at every segment a static match is preferred
    /// over a parameter, and a parameter over a catch-all.
    fn find(
        &self,
        query: &[RouteSegment],
        accept: &impl Fn(&HttpBoundary) -> bool,
    ) -> Vec<&'a HttpBoundary> {
        let Some((first, rest)) = query.split_first() else {
            return self.terminal.accepted(accept);
        };
        if let RouteSegment::Static(value) = first
            && let Some(child) = self.statics.get(value)
        {
            let found = child.find(rest, accept);
            if !found.is_empty() {
                return found;
            }
        }
        if matches!(first, RouteSegment::Static(_) | RouteSegment::Param)
            && let Some(child) = &self.param
        {
            let found = child.find(rest, accept);
            if !found.is_empty() {
                return found;
            }
        }
        self.catch_all.accepted(accept)
    }
}

/// Outcome of resolving one HTTP call against the provider index.
#[derive(Debug, PartialEq)]
enum RouteResolution<'a> {
    Unique(&'a HttpBoundary),
    Ambiguous(Vec<&'a HttpBoundary>),
    Unmatched,
}

/// Per-method tries of provider shapes.
#[derive(Debug, Default)]
struct RouteIndex<'a> {
    methods: BTreeMap<&'a str, RouteNode<'a>>,
}

impl<'a> RouteIndex<'a> {
    fn new(boundaries: &'a [HttpBoundary]) -> Self {
        let mut index = Self::default();
        for provider in boundaries
            .iter()
            .filter(|boundary| boundary.role == BoundaryRole::Provider)
        {
            index
                .methods
                .entry(provider.method.as_str())
                .or_default()
                .insert(RouteShape::parse(&provider.path).segments(), provider);
        }
        index
    }

    /// Applies the provider scope rules in order: an explicit repository restriction (mapped
    /// authority or in-process client), then the caller's own repository, then a unique provider
    /// in the workspace. Anything else is ambiguous.
    fn resolve(
        &self,
        method: &str,
        path: &str,
        caller_repository: Option<&RepoId>,
        restriction: Option<&RepoId>,
    ) -> RouteResolution<'a> {
        let Some(root) = self.methods.get(method) else {
            return RouteResolution::Unmatched;
        };
        let shape = RouteShape::parse(path);
        let mut found = match restriction {
            Some(repository) => root.find(shape.segments(), &|provider: &HttpBoundary| {
                provider.node.repo_id.as_ref() == Some(repository)
            }),
            None => root.find(shape.segments(), &|_: &HttpBoundary| true),
        };
        if found.len() > 1
            && let Some(caller) = caller_repository
        {
            let local = found
                .iter()
                .copied()
                .filter(|provider| provider.node.repo_id.as_ref() == Some(caller))
                .collect::<Vec<_>>();
            if local.len() == 1 {
                found = local;
            }
        }
        match found.as_slice() {
            [] => RouteResolution::Unmatched,
            [provider] => RouteResolution::Unique(provider),
            _ => RouteResolution::Ambiguous(found),
        }
    }
}

/// Repository restriction implied by a call scope.
#[derive(Debug, Clone, Copy)]
enum Restriction<'a> {
    /// Resolve across the workspace.
    Workspace,
    /// Resolve only inside one repository.
    Strict(&'a RepoId),
    /// Prefer one repository and fall back to the workspace when it has no match.
    Preferred(&'a RepoId),
    /// The call leaves the workspace and is never linked.
    External,
}

fn scope_restriction<'a>(
    scope: &'a CallScope,
    caller_repository: Option<&'a RepoId>,
    authorities: &'a AuthorityMap,
) -> Restriction<'a> {
    match scope {
        CallScope::Workspace => Restriction::Workspace,
        CallScope::InProcess => {
            caller_repository.map_or(Restriction::Workspace, Restriction::Strict)
        }
        CallScope::Authority(authority) => match authorities.lookup(authority) {
            AuthorityTarget::Workspace => Restriction::Workspace,
            AuthorityTarget::Declared(repository) => Restriction::Strict(repository),
            AuthorityTarget::Inferred(repository) => Restriction::Preferred(repository),
            AuthorityTarget::External => Restriction::External,
        },
        CallScope::External => Restriction::External,
    }
}

/// Outcome of resolving one scoped call.
enum CallResolution<'a> {
    Route(RouteResolution<'a>),
    External,
}

fn resolve_call<'a>(
    index: &RouteIndex<'a>,
    method: &str,
    path: &str,
    caller_repository: Option<&'a RepoId>,
    scope: &'a CallScope,
    authorities: &'a AuthorityMap,
) -> CallResolution<'a> {
    let resolution = match scope_restriction(scope, caller_repository, authorities) {
        Restriction::Workspace => index.resolve(method, path, caller_repository, None),
        Restriction::Strict(repository) => {
            index.resolve(method, path, caller_repository, Some(repository))
        }
        Restriction::Preferred(repository) => {
            match index.resolve(method, path, caller_repository, Some(repository)) {
                RouteResolution::Unmatched => index.resolve(method, path, caller_repository, None),
                resolution => resolution,
            }
        }
        Restriction::External => return CallResolution::External,
    };
    CallResolution::Route(resolution)
}

/// Links HTTP consumers (`calls_remote`), tests (`validates`), and implementation anchors
/// (`implemented_by`) to providers through one route index.
///
/// Concrete paths match provider templates; the most specific template wins and equal-shape
/// providers are narrowed by scope. Remaining ties are reported as ambiguities with their
/// candidates instead of edges. `authorities` maps the hosts named by absolute URLs to the
/// repository that serves them; calls to unknown hosts leave the workspace and are reported as
/// external rather than unmatched.
#[must_use]
pub fn link_http_routes(
    boundaries: &[HttpBoundary],
    tests: &[DeclaredTestCase],
    implementations: &[DeclaredImplementation],
    authorities: &AuthorityMap,
) -> HttpLinkResolution {
    let index = RouteIndex::new(boundaries);
    let mut sink = LinkSink::default();

    let consumers = boundaries
        .iter()
        .filter(|boundary| boundary.role == BoundaryRole::Consumer)
        .map(|consumer| RemoteCall {
            node: &consumer.node,
            method: &consumer.method,
            path: &consumer.path,
            scope: &consumer.scope,
            evidence: &consumer.evidence,
            kind: EdgeKind::CallsRemote,
            relation: "calls_remote",
        });
    let test_calls = tests.iter().map(|test| RemoteCall {
        node: &test.node,
        method: &test.method,
        path: &test.path,
        scope: &test.scope,
        evidence: &test.evidence,
        kind: EdgeKind::Validates,
        relation: "validates",
    });
    for call in consumers.chain(test_calls) {
        link_remote_call(&mut sink, &index, authorities, &call);
    }

    for implementation in implementations {
        let Some(repository) = implementation.node.repo_id.as_ref() else {
            continue;
        };
        match index.resolve(
            &implementation.method,
            &implementation.path,
            Some(repository),
            Some(repository),
        ) {
            RouteResolution::Unique(provider) => sink.link(
                &provider.node.id,
                &implementation.node.id,
                EdgeKind::ImplementedBy,
                "implemented_by",
                provider
                    .evidence
                    .confidence
                    .min(implementation.evidence.confidence),
                vec![
                    provider.evidence.id.clone(),
                    implementation.evidence.id.clone(),
                ],
            ),
            RouteResolution::Ambiguous(candidates) => {
                sink.ambiguous(&implementation.method, &implementation.path, &candidates);
            }
            RouteResolution::Unmatched => {}
        }
    }

    sink.finish()
}

/// Consumer or test call resolved against provider routes.
struct RemoteCall<'a> {
    node: &'a Node,
    method: &'a str,
    path: &'a str,
    scope: &'a CallScope,
    evidence: &'a Evidence,
    kind: EdgeKind,
    relation: &'static str,
}

fn link_remote_call<'a>(
    sink: &mut LinkSink,
    index: &RouteIndex<'a>,
    authorities: &'a AuthorityMap,
    call: &RemoteCall<'a>,
) {
    let caller = call.node.repo_id.as_ref();
    match resolve_call(
        index,
        call.method,
        call.path,
        caller,
        call.scope,
        authorities,
    ) {
        CallResolution::Route(RouteResolution::Unique(provider)) => {
            sink.report.coverage.linked += 1;
            sink.link(
                &call.node.id,
                &provider.node.id,
                call.kind,
                call.relation,
                call.evidence.confidence.min(provider.evidence.confidence),
                vec![call.evidence.id.clone(), provider.evidence.id.clone()],
            );
        }
        CallResolution::Route(RouteResolution::Ambiguous(candidates)) => {
            sink.ambiguous(call.method, call.path, &candidates);
            sink.gap(call, HttpLinkGapReason::Ambiguous, &candidates);
        }
        CallResolution::Route(RouteResolution::Unmatched) => {
            sink.gap(call, HttpLinkGapReason::NoProvider, &[]);
        }
        CallResolution::External => sink.gap(call, HttpLinkGapReason::External, &[]),
    }
}

/// Accumulates de-duplicated route edges and unresolved ambiguities.
#[derive(Debug, Default)]
struct LinkSink {
    edges: BTreeMap<EdgeId, Edge>,
    ambiguities: Vec<HttpLinkAmbiguity>,
    report: HttpLinkReport,
}

impl LinkSink {
    fn link(
        &mut self,
        source: &NodeId,
        target: &NodeId,
        kind: EdgeKind,
        relation: &str,
        confidence: f32,
        evidence: Vec<EvidenceId>,
    ) {
        let edge = Edge {
            id: EdgeId::new(stable_id(
                "edge",
                &format!("{}:{relation}:{}", source.as_str(), target.as_str()),
            )),
            source: source.clone(),
            target: target.clone(),
            kind,
            confidence,
            status: consensus_status(confidence),
            evidence,
        };
        self.edges
            .entry(edge.id.clone())
            .and_modify(|existing| merge_equivalent_edge(existing, &edge))
            .or_insert(edge);
    }

    fn ambiguous(&mut self, method: &str, path: &str, candidates: &[&HttpBoundary]) {
        self.ambiguities.push(http_link_ambiguity(
            method,
            path,
            candidates.iter().map(|candidate| &candidate.node.id),
        ));
    }

    fn gap(
        &mut self,
        call: &RemoteCall<'_>,
        reason: HttpLinkGapReason,
        candidates: &[&HttpBoundary],
    ) {
        let coverage = &mut self.report.coverage;
        match reason {
            HttpLinkGapReason::NoProvider => coverage.no_provider += 1,
            HttpLinkGapReason::Ambiguous => coverage.ambiguous += 1,
            HttpLinkGapReason::External => coverage.external += 1,
        }
        let mut candidates = candidates
            .iter()
            .map(|candidate| candidate.node.id.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        self.report.gaps.push(HttpLinkGap {
            caller: call.node.id.clone(),
            method: call.method.to_owned(),
            path: call.path.to_owned(),
            reason,
            candidates,
        });
    }

    fn finish(mut self) -> HttpLinkResolution {
        sort_http_ambiguities(&mut self.ambiguities);
        self.report.gaps.sort();
        self.report.gaps.dedup();
        HttpLinkResolution {
            edges: self.edges.into_values().collect(),
            ambiguities: self.ambiguities,
            report: self.report,
        }
    }
}

fn merge_equivalent_edge(existing: &mut Edge, candidate: &Edge) {
    existing.confidence = existing.confidence.max(candidate.confidence);
    existing.status = consensus_status(existing.confidence);
    existing.evidence.extend(candidate.evidence.iter().cloned());
    existing.evidence.sort();
    existing.evidence.dedup();
}

fn consensus_status(confidence: f32) -> EpistemicStatus {
    if confidence >= 1.0 {
        EpistemicStatus::Confirmed
    } else {
        EpistemicStatus::Inferred
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeKind, HttpLinkGapReason, RepoId};

    use super::{
        AuthorityMap, CallScope, RouteSegment, RouteShape, canonical_route, link_http_routes, normalize_authority, url_authority
    };
    use crate::{HttpBoundary, HttpConsumerConfig, extract_openapi};

    fn provider(repo: &str, method: &str, path: &str) -> HttpBoundary {
        let document = format!(
            "openapi: 3.0.3\npaths:\n  {path}:\n    {}: {{}}\n",
            method.to_ascii_lowercase()
        );
        match extract_openapi(&RepoId::new(repo), "openapi.yaml", &document) {
            Ok(mut boundaries) => boundaries.remove(0),
            Err(error) => panic!("provider fixture must be valid: {error}"),
        }
    }

    fn consumer(repo: &str, method: &str, path: &str) -> HttpBoundary {
        HttpBoundary::consumer(
            RepoId::new(repo),
            &HttpConsumerConfig {
                method: method.to_owned(),
                path: path.to_owned(),
                source: "src/client.ts".to_owned(),
            },
        )
    }

    fn linked_targets(boundaries: &[HttpBoundary]) -> Vec<String> {
        let resolution = link_http_routes(boundaries, &[], &[], &AuthorityMap::new());
        resolution
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::CallsRemote)
            .filter_map(|edge| {
                boundaries
                    .iter()
                    .find(|boundary| boundary.node.id == edge.target)
                    .map(|boundary| boundary.path.clone())
            })
            .collect()
    }

    #[test]
    fn route_shape_should_normalize_every_framework_parameter_syntax() {
        let parameter = "/orders/{}";
        for template in [
            "/orders/{id}",
            "/orders/:id",
            "/orders/<int:id>",
            "/orders/<id>",
            "/orders/{id:int}",
            "/orders/{id:[0-9]+}",
            "/orders/[id]",
            "/orders/${orderId}",
        ] {
            assert_eq!(canonical_route(template), parameter, "{template}");
        }
        for template in [
            "/files/[...slug]",
            "/files/[[...slug]]",
            "/files/{*rest}",
            "/files/{path...}",
            "/files/{rest:path}",
            "/files/<path:rest>",
            "/files/*filepath",
            "/files/:rest*",
            "/files/*",
            "/files/**",
        ] {
            assert_eq!(canonical_route(template), "/files/{*}", "{template}");
        }
        assert_eq!(canonical_route("/"), "/");
        assert_eq!(canonical_route("/orders/42?expand=true"), "/orders/42");
        assert!(RouteShape::parse("/orders/42").is_concrete());
        assert_eq!(
            RouteShape::parse("/v1/{tenant}/orders").segments(),
            &[
                RouteSegment::Static("v1".to_owned()),
                RouteSegment::Param,
                RouteSegment::Static("orders".to_owned()),
            ]
        );
    }

    #[test]
    fn concrete_path_should_link_to_the_most_specific_template() {
        let boundaries = [
            provider("repo:api", "GET", "/orders/{id}"),
            provider("repo:api", "GET", "/orders/export"),
            provider("repo:files", "GET", "/{tenant}/export"),
            provider("repo:files", "GET", "/orders/{rest:path}"),
            consumer("repo:web", "GET", "/orders/42"),
            consumer("repo:web", "GET", "/orders/export"),
            consumer("repo:web", "GET", "/orders/42/lines/7"),
        ];

        let mut targets = linked_targets(&boundaries);
        targets.sort();

        assert_eq!(
            targets,
            vec![
                "/orders/export".to_owned(),
                "/orders/{id}".to_owned(),
                "/orders/{rest:path}".to_owned(),
            ]
        );
    }

    #[test]
    fn equal_shapes_should_prefer_the_callers_repository_then_report_ambiguity() {
        let local = [
            provider("repo:web", "POST", "/orders"),
            provider("repo:api", "POST", "/orders"),
            consumer("repo:web", "POST", "/orders"),
        ];
        assert_eq!(linked_targets(&local), vec!["/orders".to_owned()]);

        let remote = [
            provider("repo:api-a", "POST", "/orders/{id}"),
            provider("repo:api-b", "POST", "/orders/:orderId"),
            consumer("repo:web", "POST", "/orders/9"),
        ];
        let resolution = link_http_routes(&remote, &[], &[], &AuthorityMap::new());
        assert_eq!(resolution.edges, Vec::new());
        assert_eq!(resolution.ambiguities.len(), 1);
        assert_eq!(resolution.ambiguities[0].path, "/orders/9");
        assert_eq!(resolution.ambiguities[0].candidates.len(), 2);
    }

    #[test]
    fn scoped_calls_should_resolve_only_inside_their_restriction() {
        let mut in_process = consumer("repo:api-b", "GET", "/health");
        in_process.scope = CallScope::InProcess;
        let mut mapped = consumer("repo:web", "GET", "/health");
        mapped.scope = CallScope::Authority("orders-api:8080".to_owned());
        let mut external = consumer("repo:web", "GET", "/health");
        external.scope = CallScope::External;
        let boundaries = [
            provider("repo:api-a", "GET", "/health"),
            provider("repo:api-b", "GET", "/health"),
            in_process,
            mapped,
            external,
        ];
        let mut authorities = AuthorityMap::new();
        authorities.declare("orders-api:8080".to_owned(), RepoId::new("repo:api-a"));

        let resolution = link_http_routes(&boundaries, &[], &[], &authorities);
        let mut pairs = resolution
            .edges
            .iter()
            .filter_map(|edge| {
                let source = boundaries.iter().find(|item| item.node.id == edge.source)?;
                let target = boundaries.iter().find(|item| item.node.id == edge.target)?;
                Some((
                    source.node.repo_id.clone()?.as_str().to_owned(),
                    target.node.repo_id.clone()?.as_str().to_owned(),
                ))
            })
            .collect::<Vec<_>>();
        pairs.sort();

        assert_eq!(
            pairs,
            vec![
                ("repo:api-b".to_owned(), "repo:api-b".to_owned()),
                ("repo:web".to_owned(), "repo:api-a".to_owned()),
            ]
        );
        assert_eq!(resolution.ambiguities, Vec::new());
        assert!(
            resolution
                .report
                .gaps
                .iter()
                .any(|call| call.reason == HttpLinkGapReason::External)
        );
    }

    fn linked_repositories(boundaries: &[HttpBoundary], authorities: &AuthorityMap) -> Vec<String> {
        link_http_routes(boundaries, &[], &[], authorities)
            .edges
            .iter()
            .filter_map(|edge| {
                let target = boundaries.iter().find(|item| item.node.id == edge.target)?;
                Some(target.node.repo_id.clone()?.as_str().to_owned())
            })
            .collect()
    }

    fn authority_consumer(authority: &str) -> HttpBoundary {
        let mut call = consumer("repo:web", "GET", "/orders/42");
        call.scope = CallScope::Authority(authority.to_owned());
        call
    }

    #[test]
    fn authorities_should_resolve_loopback_declared_inferred_and_external_hosts() {
        let providers = [
            provider("repo:api-a", "GET", "/orders/{id}"),
            provider("repo:api-b", "GET", "/orders/{id}"),
        ];
        let mut authorities = AuthorityMap::new();
        authorities.declare("orders-api".to_owned(), RepoId::new("repo:api-a"));
        authorities.infer("billing", &RepoId::new("repo:api-b"));
        authorities.infer("shared", &RepoId::new("repo:api-a"));
        authorities.infer("shared", &RepoId::new("repo:api-b"));
        let resolve = |authority: &str| {
            let mut boundaries = providers.to_vec();
            boundaries.push(authority_consumer(authority));
            linked_repositories(&boundaries, &authorities)
        };

        assert_eq!(resolve("orders-api:8080"), vec!["repo:api-a".to_owned()]);
        assert_eq!(
            resolve("billing.payments.svc.cluster.local"),
            vec!["repo:api-b".to_owned()]
        );
        assert_eq!(resolve("localhost:8080"), Vec::<String>::new());
        assert_eq!(resolve("shared:80"), Vec::<String>::new());
        assert_eq!(resolve("api.example.com"), Vec::<String>::new());

        let mut single = vec![provider("repo:api-a", "GET", "/orders/{id}")];
        single.push(authority_consumer("127.0.0.1:3000"));
        assert_eq!(
            linked_repositories(&single, &authorities),
            vec!["repo:api-a".to_owned()]
        );
    }

    #[test]
    fn inferred_authority_should_fall_back_to_the_workspace_when_its_repository_has_no_route() {
        let mut boundaries = vec![provider("repo:api-a", "GET", "/orders/{id}")];
        boundaries.push(authority_consumer("gateway:8080"));
        let mut authorities = AuthorityMap::new();
        authorities.infer("gateway", &RepoId::new("repo:edge"));
        assert_eq!(
            linked_repositories(&boundaries, &authorities),
            vec!["repo:api-a".to_owned()]
        );

        let mut strict = AuthorityMap::new();
        strict.declare("gateway".to_owned(), RepoId::new("repo:edge"));
        assert_eq!(
            linked_repositories(&boundaries, &strict),
            Vec::<String>::new()
        );
    }

    #[test]
    fn authority_normalization_should_reject_schemes_and_paths() {
        assert_eq!(
            url_authority("https://User@Orders-API.:8080/v1?x=1"),
            Some("orders-api:8080".to_owned())
        );
        assert_eq!(
            url_authority("http://[::1]:9000/x"),
            Some("[::1]:9000".to_owned())
        );
        assert_eq!(normalize_authority("http://orders"), None);
        assert_eq!(normalize_authority("orders/v1"), None);
        assert_eq!(normalize_authority("orders:http"), None);
        assert_eq!(normalize_authority("Orders"), Some("orders".to_owned()));
    }

    #[test]
    fn provider_identity_should_ignore_parameter_names() {
        let openapi = provider("repo:api", "GET", "/orders/{orderId}");
        let express = provider("repo:api", "GET", "/orders/:id");

        assert_eq!(openapi.node.id, express.node.id);
    }
}
