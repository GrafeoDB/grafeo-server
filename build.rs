fn main() {
    // Extract grafeo-engine version from Cargo.lock so we can expose it at compile time.
    let lock = std::fs::read_to_string("Cargo.lock").expect("Cargo.lock not found");
    let version =
        extract_dep_version(&lock, "grafeo-engine").expect("grafeo-engine not found in Cargo.lock");
    println!("cargo:rustc-env=GRAFEO_ENGINE_VERSION={version}");
    println!("cargo::rerun-if-changed=Cargo.lock");
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
