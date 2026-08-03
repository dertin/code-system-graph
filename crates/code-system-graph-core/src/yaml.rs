use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::ExtractionBudgets;

pub(crate) fn from_str<'de, T>(input: &'de str) -> Result<T, serde_saphyr::DeserializeError>
where
    T: Deserialize<'de>,
{
    serde_saphyr::from_str_with_options(
        input,
        serde_saphyr::options! {
            with_snippet: false,
        },
    )
}

pub(crate) fn from_multiple<T>(input: &str) -> Result<Vec<T>, serde_saphyr::DeserializeError>
where
    T: DeserializeOwned,
{
    serde_saphyr::from_multiple_with_options(
        input,
        serde_saphyr::options! {
            with_snippet: false,
        },
    )
}

pub(crate) fn from_str_with_extraction_budgets<'de, T>(
    input: &'de str,
    budgets: &ExtractionBudgets,
) -> Result<T, serde_saphyr::DeserializeError>
where
    T: Deserialize<'de>,
{
    let work = usize::try_from(budgets.max_work_units_per_artifact).unwrap_or(usize::MAX);
    let depth = usize::try_from(budgets.max_structural_depth_per_artifact).unwrap_or(usize::MAX);
    let scalar_bytes =
        usize::try_from(budgets.max_accumulated_string_bytes_per_artifact).unwrap_or(usize::MAX);
    let input_bytes = usize::try_from(budgets.max_input_bytes_per_artifact).unwrap_or(usize::MAX);
    let event_limit = work.saturating_mul(4);
    serde_saphyr::from_str_with_options(
        input,
        serde_saphyr::options! {
            with_snippet: false,
            budget: serde_saphyr::budget! {
                max_events: event_limit,
                max_aliases: work,
                max_anchors: work,
                max_depth: depth,
                max_inclusion_depth: 0,
                max_documents: 1,
                max_nodes: work,
                max_total_scalar_bytes: scalar_bytes,
                max_total_comment_bytes: input_bytes,
                max_merge_keys: work,
                enforce_alias_anchor_ratio: false,
            },
            alias_limits: serde_saphyr::alias_limits! {
                max_total_replayed_events: work,
                max_replay_stack_depth: depth,
                max_alias_expansions_per_anchor: work,
            },
        },
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_do_not_include_source_snippets() {
        let input = "password: super-secret-value\ninvalid: [";
        let error = super::from_str::<serde_json::Value>(input).expect_err("invalid YAML");
        let message = error.to_string();

        assert!(!message.contains("super-secret-value"));
        assert!(!message.contains("password:"));
    }
}
