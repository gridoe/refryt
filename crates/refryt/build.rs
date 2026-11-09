//! Builds in the release notes from `release-notes/`, newest first, for What's new in Settings →
//! About (`src/changelog.rs`). A tree without the folder builds with none.
//!
//! Embeds `refryt.ico` as the .exe's own icon resource, and the version stamp Windows shows under
//! Properties → Details — Windows only, and specifically the *target* platform, not the host
//! running this build script: cross-compiling for Windows from Linux still needs this to run, so
//! the check is `CARGO_CFG_TARGET_OS` rather than `cfg(windows)`, which would reflect the host
//! instead and never fire at all.
//!
//! The stamp is also what the MSI reads to decide whether to replace an installed `refryt.exe`
//! (`scripts/build-msi.sh` copies it into the File table), so it comes from Cargo's own version
//! rather than being kept by hand. `refryt.rc` is the template; the filled-in copy goes to
//! `OUT_DIR`, with the icon path made absolute so it still resolves from there.

fn main() {
    changelog();
    println!("cargo::rerun-if-changed=refryt.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let var = |name: &str| std::env::var(name).unwrap();
    let manifest_dir = var("CARGO_MANIFEST_DIR").replace('\\', "/");
    let numbers = format!(
        "{},{},{},0",
        var("CARGO_PKG_VERSION_MAJOR"),
        var("CARGO_PKG_VERSION_MINOR"),
        var("CARGO_PKG_VERSION_PATCH"),
    );
    let rc = std::fs::read_to_string("refryt.rc")
        .unwrap()
        .replace("../../", &format!("{manifest_dir}/../../"))
        .replace("@VERSION_NUMBERS@", &numbers)
        .replace("@VERSION@", &var("CARGO_PKG_VERSION"));
    let out = std::path::Path::new(&var("OUT_DIR")).join("refryt.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}

/// Every `release-notes/<version>.md`, newest first, as `NOTES` in `OUT_DIR/changelog.rs`.
fn changelog() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dir = std::path::Path::new(&manifest_dir).join("../../release-notes");
    println!("cargo::rerun-if-changed={}", dir.display());
    let mut notes: Vec<(Vec<u64>, String, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let version = path.file_name()?.to_str()?.strip_suffix(".md")?.to_owned();
            let parts = version
                .split('.')
                .map(|n| n.parse().ok())
                .collect::<Option<Vec<u64>>>()?;
            Some((parts, version, path.canonicalize().ok()?))
        })
        .collect();
    notes.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = String::from("pub(crate) const NOTES: &[(&str, &str)] = &[\n");
    for (_, version, path) in &notes {
        let path = path.to_str().unwrap();
        out.push_str(&format!("    ({version:?}, include_str!({path:?})),\n"));
    }
    out.push_str("];\n");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(std::path::Path::new(&out_dir).join("changelog.rs"), out).unwrap();
}
