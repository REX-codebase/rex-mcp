use rex_ultra::phase5::{audit, build_index, build_twin, profile, ProfileSample};
use std::env;
use std::path::Path;
fn main() {
    let root = env::args().nth(1).unwrap_or_else(|| ".".into());
    let root = Path::new(&root);
    let twin = build_twin(root).unwrap_or_else(|e| {
        eprintln!("twin: {e}");
        std::process::exit(2)
    });
    let index = build_index(root, &twin).unwrap_or_else(|e| {
        eprintln!("index: {e}");
        std::process::exit(2)
    });
    let report = audit(root, &twin).unwrap_or_else(|e| {
        eprintln!("audit: {e}");
        std::process::exit(2)
    });
    let bytes = twin.files.iter().map(|f| f.bytes).sum();
    let prof = profile(
        &twin,
        vec![
            ProfileSample {
                name: "repository_bytes".into(),
                value: bytes,
                unit: "bytes".into(),
                budget: 512 * 1024 * 1024,
                command: "digital-twin byte census".into(),
            },
            ProfileSample {
                name: "repository_files".into(),
                value: twin.files.len() as u64,
                unit: "files".into(),
                budget: 20_000,
                command: "digital-twin file census".into(),
            },
            ProfileSample {
                name: "semantic_symbols".into(),
                value: index.symbols.len() as u64,
                unit: "symbols".into(),
                budget: 100_000,
                command: "semantic index".into(),
            },
        ],
    );
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"twin":twin,"index":index,"audit":report,"profiles":prof})
        )
        .unwrap()
    );
}
