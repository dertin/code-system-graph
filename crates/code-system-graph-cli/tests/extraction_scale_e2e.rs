//! Release-mode extraction acceptance for a representative 5,000-file workspace.

use std::time::{Duration, Instant};

use code_system_graph::scan_workspace;
use code_system_graph_core::{
    ExtractionBudgets, ExtractionTracker, SourceSyntaxLanguage, extract_generated_client_metadata, extract_graphql_document, extract_package_manifest, extract_protobuf, inspect_source_syntax
};

const FILES_PER_KIND: usize = 1_000;
const FILE_COUNT: usize = FILES_PER_KIND * 5;

#[test]
#[ignore = "run explicitly with --release for the 5,000-file extraction workload"]
fn representative_workspace_should_scan_5000_files_with_bounded_resources() -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        anyhow::bail!("run this acceptance test with --release");
    }
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    std::fs::create_dir(&repository)?;
    let mut artifact_samples = Vec::with_capacity(FILE_COUNT);

    for index in 0..FILES_PER_KIND {
        let graphql = format!("type Type{index:04} {{ id: ID! }}\n");
        std::fs::write(
            repository.join(format!("type-{index:04}.graphql")),
            &graphql,
        )?;
        artifact_samples.push(measure(|| {
            extract_graphql_document("schema.graphql", &graphql).map(|_| ())
        })?);

        let protobuf =
            format!("syntax = \"proto3\"; message Message{index:04} {{ string id = 1; }}\n");
        std::fs::write(
            repository.join(format!("message-{index:04}.proto")),
            &protobuf,
        )?;
        artifact_samples.push(measure(|| {
            extract_protobuf("message.proto", &protobuf).map(|_| ())
        })?);

        let package_directory = repository.join(format!("packages/package-{index:04}"));
        std::fs::create_dir_all(&package_directory)?;
        let maven = format!(
            "<project><groupId>example</groupId><artifactId>package-{index:04}</artifactId><version>1.0.0</version></project>"
        );
        std::fs::write(package_directory.join("pom.xml"), &maven)?;
        artifact_samples.push(measure(|| {
            extract_package_manifest("pom.xml", &maven).map(|_| ())
        })?);

        let javascript = format!("fetch('/orders/{index:04}');\n");
        std::fs::write(
            repository.join(format!("client-{index:04}.js")),
            &javascript,
        )?;
        artifact_samples.push(measure(|| {
            let mut tracker =
                ExtractionTracker::new("client.js", "tree-sitter", &ExtractionBudgets::default());
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "client.js",
                &javascript,
                &mut tracker,
            )
            .map(|_| ())
        })?);

        let generated_directory =
            repository.join(format!("generated/client-{index:04}/.openapi-generator"));
        std::fs::create_dir_all(&generated_directory)?;
        let files = format!("src/generated-{index:04}.ts\n");
        std::fs::write(generated_directory.join("FILES"), &files)?;
        artifact_samples.push(measure(|| {
            let mut tracker = ExtractionTracker::new(
                ".openapi-generator/FILES",
                "generated-client",
                &ExtractionBudgets::default(),
            );
            extract_generated_client_metadata(".openapi-generator/FILES", &files, &mut tracker)
                .map(|_| ())
        })?);
    }

    let manifest = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(
        &manifest,
        "version: 1\nname: extraction-scale\nrepos:\n  representative:\n    path: repository\n",
    )?;
    let started = Instant::now();
    let summary = scan_workspace(&manifest, &database)?;
    let total = started.elapsed();
    artifact_samples.sort_unstable();
    let p50 = percentile(&artifact_samples, 50);
    let p95 = percentile(&artifact_samples, 95);
    let p99 = percentile(&artifact_samples, 99);
    let peak_memory_kib = linux_peak_memory_kib();

    eprintln!(
        "extraction_scale files={FILE_COUNT} discovered_invocations={} total_ms={} \
         artifact_p50_us={} artifact_p95_us={} artifact_p99_us={} peak_memory_kib={}",
        summary.discovered_input_count,
        total.as_millis(),
        p50.as_micros(),
        p95.as_micros(),
        p99.as_micros(),
        peak_memory_kib.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
    );
    assert_eq!(artifact_samples.len(), FILE_COUNT);
    assert!(summary.discovered_input_count >= FILE_COUNT);
    Ok(())
}

fn measure<E>(operation: impl FnOnce() -> Result<(), E>) -> Result<Duration, E> {
    let started = Instant::now();
    operation()?;
    Ok(started.elapsed())
}

fn percentile(samples: &[Duration], percentage: usize) -> Duration {
    let index = samples
        .len()
        .saturating_mul(percentage)
        .div_ceil(100)
        .saturating_sub(1);
    samples[index]
}

fn linux_peak_memory_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}
