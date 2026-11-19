//! Embed each catalog under locales/<language-tag>/cli.ftl (ADR-0005).
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=locales");
    let mut catalogs = Vec::new();
    for entry in fs::read_dir("locales").expect("read CLI locales") {
        let entry = entry.expect("read locale directory entry");
        if entry.file_type().unwrap().is_dir() {
            let tag = entry
                .file_name()
                .into_string()
                .expect("locale tag is UTF-8");
            let source = fs::read_to_string(entry.path().join("cli.ftl")).expect("read cli.ftl");
            catalogs.push((tag, source));
        }
    }
    catalogs.sort();
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("catalogs.rs");
    fs::write(output, format!("&{catalogs:?}")).expect("write embedded catalogs");
}
