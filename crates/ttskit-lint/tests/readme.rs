//! The README documents every rule.

#[test]
fn readme_lists_every_rule_id() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("README.md");
    let readme = std::fs::read_to_string(&path).expect("README.md is readable");
    for rule in ttskit_lint::RULES {
        let row = format!("| `{}` | {} |", rule.id, rule.default.as_str());
        assert!(
            readme.contains(&row),
            "README.md has no table row `{row}` for rule {}",
            rule.id
        );
    }
}
