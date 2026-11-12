//! Shader assembly shared by every probe mode.

/// Reads the compositor's WGSL straight out of `freya-nle-render/src/lib.rs`, so the probe
/// needs no intermediate copy and runs from a clean checkout.
pub fn compositor_shader() -> String {
    pub fn repo_root() -> std::path::PathBuf {
        let mut current = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        loop {
            if current.join("rust").join("Cargo.toml").is_file() {
                return current;
            }
            current = current
                .parent()
                .expect("the probe lives inside the repository")
                .to_path_buf();
        }
    }
    let path = repo_root().join("rust").join("freya-nle-render").join("src").join("lib.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    const OPEN: &str = "const SHADER: &str = r#\"";
    const CLOSE: &str = "\"#;";
    let start = source
        .find(OPEN)
        .unwrap_or_else(|| panic!("{} does not declare const SHADER", path.display()))
        + OPEN.len();
    let end = source[start..]
        .find(CLOSE)
        .unwrap_or_else(|| panic!("unterminated SHADER constant in {}", path.display()))
        + start;
    source[start..end].to_owned()
}

/// Reads the compositor's WGSL straight out of `freya-nle-render/src/lib.rs`, so the probe
/// needs no intermediate copy and runs from a clean checkout.
/// The checkout the probes belong to, found by walking up to the workspace root.
pub fn repo_root() -> std::path::PathBuf {
    let mut current = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if current.join("rust").join("Cargo.toml").is_file() {
            return current;
        }
        current = current
            .parent()
            .expect("the probe lives inside the repository")
            .to_path_buf();
    }
}
