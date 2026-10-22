//! Byte-preserving git paths, independent of the host filesystem (ADR-0005).
//!
//! Construct git trees directly from these bytes. Checking the paths out would
//! lose collision cases on case-insensitive or normalizing filesystems.

/// A leaf entry in the corpus, including its exact git mode and blob contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathFixture {
    pub name: &'static str,
    pub path: &'static [u8],
    pub mode: u32,
    pub contents: &'static [u8],
}

/// These entries coexist in one tree. Paths must not be decoded or normalized.
pub const PATH_CORPUS: &[PathFixture] = &[
    PathFixture {
        name: "non_utf8",
        path: b"caf\xe9.txt",
        mode: 0o100644,
        contents: b"Latin-1 path\n",
    },
    PathFixture {
        name: "nfc",
        path: b"caf\xc3\xa9.txt",
        mode: 0o100644,
        contents: b"NFC path\n",
    },
    PathFixture {
        name: "nfd",
        path: b"cafe\xcc\x81.txt",
        mode: 0o100644,
        contents: b"NFD path\n",
    },
    PathFixture {
        name: "case_upper",
        path: b"README",
        mode: 0o100644,
        contents: b"upper case\n",
    },
    PathFixture {
        name: "case_lower",
        path: b"readme",
        mode: 0o100644,
        contents: b"lower case\n",
    },
    PathFixture {
        name: "executable",
        path: b"bin/run",
        mode: 0o100755,
        contents: b"#!/bin/sh\nexit 0\n",
    },
    PathFixture {
        name: "symlink",
        path: b"readme-link",
        mode: 0o120000,
        contents: b"README",
    },
    PathFixture {
        name: "cjk",
        path: "docs/路徑.txt".as_bytes(),
        mode: 0o100644,
        contents: "中文內容\n".as_bytes(),
    },
    PathFixture {
        name: "delimiters",
        path: b"with\ttab\nand-newline",
        mode: 0o100644,
        contents: b"\x00\xff\x01\n",
    },
];

/// Fixture names whose paths collide after Unicode normalization.
pub const NORMALIZATION_COLLISION: (&str, &str) = ("nfc", "nfd");

/// Fixture names whose paths collide on case-insensitive filesystems.
pub const CASE_COLLISION: (&str, &str) = ("case_upper", "case_lower");
