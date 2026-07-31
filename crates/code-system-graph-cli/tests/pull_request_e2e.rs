//! End-to-end acceptance gates for opt-in remote pull-request policy.

use code_system_graph::{
    ApplicationError, PullRequestInput, PullRequestListInput, inspect_pull_request, list_pull_requests
};
use code_system_graph_core::{PullRequestError, PullRequestListState, PullRequestProviderKind};
use tokio_util::sync::CancellationToken;

fn input(provider: PullRequestProviderKind, consent: bool) -> PullRequestInput {
    PullRequestInput {
        provider,
        owner: "example".to_owned(),
        repository: "service".to_owned(),
        number: 7,
        consent_to_remote_access: consent,
    }
}

#[tokio::test]
async fn github_should_fail_closed_before_network_when_provider_is_disabled() {
    let error = inspect_pull_request(
        &input(PullRequestProviderKind::GitHub, true),
        false,
        Some("must-not-appear".to_owned()),
        None,
    )
    .await
    .expect_err("disabled GitHub provider");
    assert!(matches!(
        error,
        ApplicationError::PullRequest(PullRequestError::Disabled)
    ));
    assert!(!format!("{error:?}").contains("must-not-appear"));
}

#[tokio::test]
async fn bitbucket_should_require_per_request_consent_before_network() {
    let error = inspect_pull_request(
        &input(PullRequestProviderKind::BitbucketCloud, false),
        true,
        Some("must-not-appear".to_owned()),
        Some("user@example.invalid".to_owned()),
    )
    .await
    .expect_err("Bitbucket consent gate");
    assert!(matches!(
        error,
        ApplicationError::PullRequest(PullRequestError::ConsentRequired)
    ));
    assert!(!format!("{error:?}").contains("must-not-appear"));
}

#[tokio::test]
async fn bitbucket_data_center_should_remain_explicitly_unsupported() {
    let error = inspect_pull_request(
        &input(PullRequestProviderKind::BitbucketDataCenter, true),
        true,
        None,
        None,
    )
    .await
    .expect_err("unsupported Bitbucket Data Center");
    assert!(matches!(
        error,
        ApplicationError::PullRequest(PullRequestError::InvalidConfiguration(_))
    ));
}

#[tokio::test]
async fn pull_request_list_should_fail_closed_before_remote_access() {
    let input = PullRequestListInput {
        provider: PullRequestProviderKind::GitHub,
        owner: "example".to_owned(),
        repository: "service".to_owned(),
        state: PullRequestListState::Open,
        cursor: None,
        limit: 25,
        consent_to_remote_access: true,
    };
    let error = list_pull_requests(
        &input,
        false,
        Some("must-not-appear".to_owned()),
        None,
        CancellationToken::new(),
    )
    .await
    .expect_err("disabled list provider");

    assert!(matches!(
        error,
        ApplicationError::PullRequest(PullRequestError::Disabled)
    ));
    assert!(!format!("{error:?}").contains("must-not-appear"));
}

#[test]
fn pull_request_overlap_cli_should_remain_local_and_deterministic() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let semantic = |fingerprint: &str, dependency: Option<&str>| {
        serde_json::json!({
            "fingerprint": fingerprint,
            "files": ["src/shared.rs"],
            "contracts": [],
            "services": [],
            "communities": [],
            "depends_on": dependency.into_iter().collect::<Vec<_>>(),
            "readiness": "ready"
        })
    };
    let left = temporary.path().join("left.json");
    let right = temporary.path().join("right.json");
    std::fs::write(&left, serde_json::to_vec(&semantic("left", None))?)?;
    std::fs::write(
        &right,
        serde_json::to_vec(&semantic("right", Some("left")))?,
    )?;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args([
            "pr",
            "overlap",
            "--left",
            left.to_string_lossy().as_ref(),
            "--right",
            right.to_string_lossy().as_ref(),
        ])
        .output()?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    assert!(output.status.success());
    assert_eq!(report["overlap"]["kind"], "file");
    assert_eq!(report["order"]["order"], "left_first");
    Ok(())
}
