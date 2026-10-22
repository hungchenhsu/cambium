use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use cambium_backend_conformance::paths::{CASE_COLLISION, NORMALIZATION_COLLISION, PATH_CORPUS};

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    // Ignore caller git state, including alternate object stores and config.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("no-global-config"))
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.ignorecase=false",
            "-c",
            "core.precomposeUnicode=false",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git must be installed to verify the path corpus");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn object_id(output: Vec<u8>) -> String {
    let id = String::from_utf8(output).unwrap().trim().to_owned();
    assert_eq!(id.len(), 40);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    id
}

#[test]
fn corpus_keeps_collision_and_non_utf8_cases_distinct() {
    let fixture = |name| PATH_CORPUS.iter().find(|case| case.name == name).unwrap();
    assert!(std::str::from_utf8(fixture("non_utf8").path).is_err());
    let (nfc, nfd) = NORMALIZATION_COLLISION;
    assert_eq!(fixture(nfc).path, "café.txt".as_bytes());
    assert_eq!(fixture(nfd).path, "cafe\u{301}.txt".as_bytes());
    assert_ne!(fixture(nfc).path, fixture(nfd).path);
    let (upper, lower) = CASE_COLLISION;
    assert_ne!(fixture(upper).path, fixture(lower).path);
    assert_eq!(
        fixture(upper).path.to_ascii_lowercase(),
        fixture(lower).path.to_ascii_lowercase()
    );
}

#[test]
fn git_round_trip_preserves_every_path_mode_blob_and_tree_id() {
    let repository = tempfile::tempdir().unwrap();
    let directory = repository.path();
    git(
        directory,
        &["init", "--bare", "--object-format=sha1", "--template="],
        b"",
    );

    let mut index = Vec::new();
    let mut expected = BTreeMap::new();
    for fixture in PATH_CORPUS {
        let id = object_id(git(
            directory,
            &["hash-object", "-w", "--stdin"],
            fixture.contents,
        ));
        index.extend_from_slice(format!("{:o} {id}\t", fixture.mode).as_bytes());
        index.extend_from_slice(fixture.path);
        index.push(0);
        assert!(
            expected
                .insert(fixture.path.to_vec(), (fixture.mode, id))
                .is_none()
        );
    }
    git(directory, &["update-index", "-z", "--index-info"], &index);
    let tree = object_id(git(directory, &["write-tree"], b""));

    let listing = git(directory, &["ls-tree", "-r", "-z", &tree], b"");
    let mut actual = BTreeMap::new();
    for entry in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let separator = entry.iter().position(|byte| *byte == b'\t').unwrap();
        let header = std::str::from_utf8(&entry[..separator]).unwrap();
        let fields: Vec<_> = header.split(' ').collect();
        assert_eq!(fields[1], "blob");
        let path = entry[separator + 1..].to_vec();
        let mode = u32::from_str_radix(fields[0], 8).unwrap();
        assert!(actual.insert(path, (mode, fields[2].to_owned())).is_none());
    }
    assert_eq!(actual, expected);
    for fixture in PATH_CORPUS {
        let (_, id) = &actual[fixture.path];
        assert_eq!(
            git(directory, &["cat-file", "blob", id], b""),
            fixture.contents
        );
    }

    let raw = git(directory, &["cat-file", "tree", &tree], b"");
    assert_eq!(
        object_id(git(
            directory,
            &["hash-object", "-t", "tree", "--stdin"],
            &raw
        )),
        tree
    );
    // Rebuild from the raw, NUL-delimited listing with an empty index.
    git(directory, &["read-tree", "--empty"], b"");
    git(directory, &["update-index", "-z", "--index-info"], &listing);
    assert_eq!(object_id(git(directory, &["write-tree"], b"")), tree);
}
