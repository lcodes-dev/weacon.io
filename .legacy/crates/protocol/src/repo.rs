use crate::token::token_id;

token_id!(
    /// A git repository the owner registered for a workspace on their machine, 1–64 ASCII
    /// letters, digits, `-` or `_`.
    RepoId,
    "not a repo id"
);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn repo_ids_are_plain_tokens_checked_on_arrival() {
        assert_eq!(RepoId::try_from("a/b".to_owned()), Err("a/b".to_owned()));
        assert_eq!("../x".parse::<RepoId>(), Err("not a repo id"));
        serde_json::from_value::<RepoId>(json!("-rf x")).unwrap_err();
        assert_eq!(
            serde_json::from_value::<RepoId>(json!("site_2"))
                .unwrap()
                .as_str(),
            "site_2"
        );
    }
}
