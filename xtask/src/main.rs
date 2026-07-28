use std::fs;
use std::path::Path;

/// The Win32 items the runtime crate needs. `windows-bindgen` pulls in whatever these depend on.
const FILTER: &[&str] = &[
    "EventRegister",
    "EventUnregister",
    "EventWriteTransfer",
    "EVENT_DATA_DESCRIPTOR",
    "EVENT_FILTER_DESCRIPTOR",
    "EVENT_CONTROL_CODE_ENABLE_PROVIDER",
    "EVENT_CONTROL_CODE_DISABLE_PROVIDER",
    "EVENT_DATA_DESCRIPTOR_TYPE_NONE",
    "ERROR_INVALID_DATA",
    "ERROR_ARITHMETIC_OVERFLOW",
];

/// Public ABI types supplied by the runtime crate instead of generated here.
const REFERENCES: &[&str] = &["crate,flat,Windows.Win32.System.Diagnostics.Etw.EVENT_DESCRIPTOR"];

fn main() {
    let mut args = std::env::args();
    let _program = args.next();

    match (args.next().as_deref(), args.next()) {
        (Some("regenerate-bindings"), None) => regenerate_bindings(),
        (Some("-h" | "--help"), None) => print_help(),
        (None, None) => {
            print_help();
            std::process::exit(2);
        }
        _ => {
            eprintln!("error: unknown xtask command or unexpected arguments");
            print_help();
            std::process::exit(2);
        }
    }
}

fn print_help() {
    eprintln!(
        "Repository maintenance tasks

Usage:
    cargo xtask <command>

Commands:
    regenerate-bindings    Regenerate the vendored Windows bindings"
    );
}

fn regenerate_bindings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must remain directly under the repository root");
    let output_dir = root.join("target").join("regenerate-bindings");
    fs::create_dir_all(&output_dir).expect("failed to create generator output directory");

    let generated = output_dir.join("bindings.rs");
    let mut args = vec![
        "--out",
        path_str(&generated),
        "--flat",
        "--sys",
        "--no-deps",
        "--no-allow",
    ];
    for reference in REFERENCES {
        args.extend_from_slice(&["--reference", reference]);
    }
    args.push("--filter");
    args.extend_from_slice(FILTER);
    windows_bindgen::bindgen(args).unwrap();

    fs::copy(
        &generated,
        root.join("etw-wrapper").join("src").join("bindings.rs"),
    )
    .expect("failed to vendor Windows bindings");
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("repository path must be valid UTF-8")
}
