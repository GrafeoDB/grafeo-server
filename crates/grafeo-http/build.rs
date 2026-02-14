fn main() {
    // Extract grafeo-engine version from Cargo.lock so health endpoints can report it.
    // Walk up from CARGO_MANIFEST_DIR to find the workspace-root Cargo.lock.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut dir = std::path::PathBuf::from(&manifest_dir);
    let lock_contents = loop {
        let candidate = dir.join("Cargo.lock");
        if let Ok(contents) = std::fs::read_to_string(&candidate) {
            println!("cargo::rerun-if-changed={}", candidate.display());
            break contents;
        }
        assert!(
            dir.pop(),
            "Cargo.lock not found in any ancestor of {manifest_dir}"
        );
    };
    let version = extract_dep_version(&lock_contents, "grafeo-engine")
        .expect("grafeo-engine not found in Cargo.lock");
    println!("cargo:rustc-env=GRAFEO_ENGINE_VERSION={version}");
}

fn extract_dep_version<'a>(lock: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("name = \"{name}\"");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == needle
            && let Some(ver_line) = lines.next()
        {
            return ver_line
                .trim()
                .strip_prefix("version = \"")?
                .strip_suffix('"');
        }
    }
    None
}
