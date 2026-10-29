//! Embeds the program files from `DOWNPOUR_PAYLOAD_DIR` (staged by
//! scripts/build-installer.ps1) into the installer. Without the variable the
//! installer builds with an empty payload and says so when run, so
//! `cargo build --workspace` keeps working during development.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Files the installer cannot work without.
const REQUIRED: [&str; 3] = ["downpour.exe", "downpour-host.exe", "downpour-cli.exe"];

fn main() {
    embed_resources();
    println!("cargo:rerun-if-env-changed=DOWNPOUR_PAYLOAD_DIR");
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("payload.rs");
    let mut files = Vec::new();
    if let Some(dir) = std::env::var_os("DOWNPOUR_PAYLOAD_DIR") {
        let dir = PathBuf::from(dir);
        collect(&dir, &dir, &mut files);
        for required in REQUIRED {
            assert!(
                files.iter().any(|(relative, _)| relative == required),
                "DOWNPOUR_PAYLOAD_DIR ({}) has no {required}",
                dir.display()
            );
        }
    }
    files.sort();
    let mut code = String::from("pub static FILES: &[PayloadFile] = &[\n");
    for (relative, absolute) in &files {
        println!("cargo:rerun-if-changed={}", absolute.display());
        writeln!(
            code,
            "    PayloadFile {{ path: {relative:?}, bytes: include_bytes!({:?}) }},",
            absolute.display().to_string()
        )
        .expect("writing to a String cannot fail");
    }
    code.push_str("];\n");
    std::fs::write(out, code).expect("write payload.rs");
}

fn embed_resources() {
    downpour_build::embed_windows_resources(downpour_build::Executable {
        file_name: "Downpour-Setup.exe",
        description: "Downpour Setup",
        assembly_name: "Downpour.Setup",
    });
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            collect(root, &path, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("walked from root")
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        files.push((relative, path));
    }
}
