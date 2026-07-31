use serde::Deserialize;
use serde::de::DeserializeOwned;

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
