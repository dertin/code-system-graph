#![no_main]

use code_system_graph_core::{
    extract_asyncapi, extract_data_artifact, extract_docker_compose, extract_graphql_document, extract_kubernetes, extract_openapi, extract_package_manifest, extract_protobuf, extract_safe_config, extract_terraform, parse_go_source, parse_java_source, parse_javascript_source, parse_manifest, parse_python_source, parse_rust_source, parse_typescript_source
};
use code_system_graph_model::RepoId;
use libfuzzer_sys::fuzz_target;

const MAX_FUZZ_INPUT_BYTES: usize = 2 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_INPUT_BYTES {
        return;
    }
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let repo_id = RepoId::new("repo:fuzz");
    let _ = parse_manifest(input);
    let _ = extract_openapi(&repo_id, "openapi.yaml", input);
    let _ = extract_asyncapi("asyncapi.yaml", input);
    let _ = extract_graphql_document("schema.graphql", input);
    let _ = extract_protobuf("contract.proto", input);
    let _ = extract_data_artifact("migration.sql", input);
    let _ = extract_docker_compose("compose.yaml", input);
    let _ = extract_kubernetes("deployment.yaml", input);
    let _ = extract_terraform("main.tf", input);
    let _ = extract_safe_config("config.json", input);
    let _ = extract_package_manifest("Cargo.toml", input);
    let _ = extract_package_manifest("Cargo.lock", input);
    let _ = extract_package_manifest("package.json", input);
    let _ = extract_package_manifest("pom.xml", input);
    let _ = parse_rust_source(input);
    let _ = parse_python_source(input);
    let _ = parse_javascript_source(input);
    let _ = parse_typescript_source(input);
    let _ = parse_go_source(input);
    let _ = parse_java_source(input);
});
