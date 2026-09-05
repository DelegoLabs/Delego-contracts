use std::fs;
use std::path::PathBuf;

const CONTRACT_CRATES: &[&str] = &[
    "delegation_registry",
    "escrow",
    "permissions",
    "reputation",
    "marketplace",
];

const EXPECTED_ATTRIBUTE: &str = "#![cfg_attr(not(test), no_std)]";

#[test]
fn test_no_std_attribute_consistency() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("CARGO_MANIFEST_DIR should have a parent workspace directory");

    let mut failures = Vec::new();

    for crate_name in CONTRACT_CRATES {
        let lib_path = workspace_root.join(crate_name).join("src").join("lib.rs");
        let content = fs::read_to_string(&lib_path)
            .unwrap_or_else(|err| panic!("Failed to read {}: {}", lib_path.display(), err));

        // Only match inner crate attributes (lines starting with `#!`)
        let no_std_attrs: Vec<&str> = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| line.starts_with("#!") && line.contains("no_std"))
            .collect();

        if no_std_attrs.len() != 1 || no_std_attrs[0] != EXPECTED_ATTRIBUTE {
            failures.push(format!(
                "Crate '{}' ({}) expected exactly one '{}' attribute line, but found: {:?}",
                crate_name,
                lib_path.display(),
                EXPECTED_ATTRIBUTE,
                no_std_attrs
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "no_std consistency check failed for one or more crates:\n{}",
        failures.join("\n")
    );
}
