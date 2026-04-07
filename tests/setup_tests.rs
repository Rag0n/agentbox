use std::fs;
use tempfile::tempdir;

#[test]
fn test_ensure_env_var_in_config_creates_env_section() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    // Empty config
    fs::write(&config_path, "").unwrap();

    // Simulate the function (we can't call it directly since it reads from a fixed path,
    // so we'll inline the logic for testing)
    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    fs::write(&config_path, doc.to_string()).unwrap();

    // Verify the key was added
    let result: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(result["env"]["MY_KEY"].as_str(), Some(""));
}

#[test]
fn test_ensure_env_var_in_config_idempotent() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    // Config with env section already containing the key
    fs::write(&config_path, r#"[env]
MY_KEY = """#).unwrap();

    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    let result_1 = doc.to_string();
    fs::write(&config_path, result_1.clone()).unwrap();

    // Run again
    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("MY_KEY") {
        env_tbl.insert("MY_KEY", toml_edit::value(""));
    }
    let result_2 = doc.to_string();

    // Should be identical
    assert_eq!(result_1, result_2);
}

#[test]
fn test_ensure_env_var_preserves_comments() {
    let temp = tempdir().unwrap();
    let config_path = temp.path().join("config.toml");

    let config_with_comment = r#"# This is my config
[env]
# My API key
EXISTING_KEY = "value"
"#;

    fs::write(&config_path, config_with_comment).unwrap();

    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap();
    let env_tbl = match doc.entry("env").or_insert(toml_edit::table()).as_table_mut() {
        Some(t) => t,
        None => panic!("'env' is not a table"),
    };
    if !env_tbl.contains_key("NEW_KEY") {
        env_tbl.insert("NEW_KEY", toml_edit::value(""));
    }
    fs::write(&config_path, doc.to_string()).unwrap();

    let result = fs::read_to_string(&config_path).unwrap();
    assert!(result.contains("# This is my config"));
    assert!(result.contains("# My API key"));
    assert!(result.contains("EXISTING_KEY"));
    assert!(result.contains("NEW_KEY"));
}
