//! Byte-preserving views computed from verified raw git objects (ADR-0007).

use std::collections::BTreeMap;

use cambium_backend::{Backend, BackendError, BackendRepositoryId, ObjectKind, RawObject};
use cambium_protocol::ObjectId;
use sha1::{Digest as _, Sha1};
use unicode_normalization::UnicodeNormalization as _;

const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 256;
const MAX_PATH_BYTES: usize = 65_536;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEntry {
    pub path: Vec<u8>,
    pub mode: u32,
    pub object_id: ObjectId,
}

impl TreeEntry {
    pub fn is_tree(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Difference {
    pub path: Vec<u8>,
    pub old: Option<TreeEntry>,
    pub new: Option<TreeEntry>,
}

pub struct GitObjects<'a> {
    pub backend: &'a dyn Backend,
    pub repository: &'a BackendRepositoryId,
}

impl GitObjects<'_> {
    /// The parser trusts neither the backend's object ID nor a lossy listing.
    pub async fn read(&self, id: &ObjectId) -> Result<RawObject, BackendError> {
        let object = self.backend.read_object(self.repository, id).await?;
        let mut hash = Sha1::new();
        hash.update(format!("{} {}\0", object.kind.as_str(), object.bytes.len()));
        hash.update(&object.bytes);
        if format!("{:x}", hash.finalize()) != id.as_str() {
            return Err(BackendError::Internal);
        }
        Ok(object)
    }

    pub async fn resolve(&self, revision: &str) -> Result<ObjectId, BackendError> {
        if let Ok(id) = revision.parse() {
            return Ok(id);
        }
        let refs = self.backend.list_refs(self.repository, &[]).await?;
        let name = if revision == "HEAD" {
            refs.head
        } else if revision.starts_with("refs/") {
            revision.to_owned()
        } else {
            format!("refs/heads/{revision}")
        };
        refs.refs
            .into_iter()
            .find(|item| item.name == name)
            .map(|item| item.object_id)
            .ok_or(BackendError::ObjectNotFound)
    }

    pub async fn root_tree(&self, id: &ObjectId) -> Result<ObjectId, BackendError> {
        let mut current = id.clone();
        for _ in 0..32 {
            let object = self.read(&current).await?;
            current = match object.kind {
                ObjectKind::Tree => return Ok(current),
                ObjectKind::Commit => {
                    let commit =
                        gix_object::CommitRef::from_bytes(&object.bytes, gix_hash::Kind::Sha1)
                            .map_err(|_| BackendError::Internal)?;
                    commit
                        .tree()
                        .to_string()
                        .parse()
                        .map_err(|_| BackendError::Internal)?
                }
                ObjectKind::Tag => {
                    let tag = gix_object::TagRef::from_bytes(&object.bytes, gix_hash::Kind::Sha1)
                        .map_err(|_| BackendError::Internal)?;
                    tag.target()
                        .to_string()
                        .parse()
                        .map_err(|_| BackendError::Internal)?
                }
                ObjectKind::Blob => {
                    return Err(invalid("revision", "Expected a commit, tree, or tag"));
                }
            };
        }
        Err(invalid("revision", "Tag chain exceeds the traversal limit"))
    }

    async fn entries(&self, tree: &ObjectId) -> Result<Vec<TreeEntry>, BackendError> {
        let object = self.read(tree).await?;
        if object.kind != ObjectKind::Tree {
            return Err(BackendError::Internal);
        }
        let tree = gix_object::TreeRef::from_bytes(&object.bytes, gix_hash::Kind::Sha1)
            .map_err(|_| BackendError::Internal)?;
        tree.entries
            .into_iter()
            .map(|entry| {
                let path = entry.filename.to_vec();
                if path.is_empty() || path.contains(&b'/') || path == b"." || path == b".." {
                    return Err(BackendError::Internal);
                }
                Ok(TreeEntry {
                    path,
                    mode: entry.mode.value() as u32,
                    object_id: entry
                        .oid
                        .to_string()
                        .parse()
                        .map_err(|_| BackendError::Internal)?,
                })
            })
            .collect()
    }

    pub async fn file(
        &self,
        revision: &ObjectId,
        path: &[u8],
    ) -> Result<(TreeEntry, Vec<u8>), BackendError> {
        validate_path(path)?;
        let components: Vec<_> = path.split(|byte| *byte == b'/').collect();
        if components.len() > MAX_DEPTH {
            return Err(invalid("path", "Path exceeds the traversal limit"));
        }
        let mut tree = self.root_tree(revision).await?;
        for (index, component) in components.iter().enumerate() {
            let mut entry = self
                .entries(&tree)
                .await?
                .into_iter()
                .find(|entry| entry.path == *component)
                .ok_or(BackendError::ObjectNotFound)?;
            if index + 1 == components.len() {
                if !matches!(entry.mode & 0o170000, 0o100000 | 0o120000) {
                    return Err(invalid("path", "Expected a file or symbolic link"));
                }
                let blob = self.read(&entry.object_id).await?;
                if blob.kind != ObjectKind::Blob {
                    return Err(BackendError::Internal);
                }
                entry.path = path.to_vec();
                return Ok((entry, blob.bytes));
            }
            if !entry.is_tree() {
                return Err(BackendError::ObjectNotFound);
            }
            tree = entry.object_id;
        }
        Err(BackendError::ObjectNotFound)
    }

    /// Returns every visited entry or an explicit limit error, never a partial tree.
    pub async fn tree(
        &self,
        revision: &ObjectId,
        recursive: bool,
    ) -> Result<Vec<TreeEntry>, BackendError> {
        let root = self.root_tree(revision).await?;
        let mut pending = vec![(root, Vec::new(), 0)];
        let mut result = Vec::new();
        while let Some((tree, prefix, depth)) = pending.pop() {
            if depth > MAX_DEPTH {
                return Err(invalid("revision", "Tree exceeds the traversal limit"));
            }
            for mut entry in self.entries(&tree).await? {
                let mut path = prefix.clone();
                path.extend_from_slice(&entry.path);
                if path.len() > MAX_PATH_BYTES {
                    return Err(invalid("path", "Path exceeds the traversal limit"));
                }
                if recursive && entry.is_tree() {
                    let mut child_prefix = path.clone();
                    child_prefix.push(b'/');
                    pending.push((entry.object_id.clone(), child_prefix, depth + 1));
                }
                entry.path = path;
                result.push(entry);
                if result.len() > MAX_ENTRIES {
                    return Err(invalid("revision", "Tree exceeds the entry limit"));
                }
            }
        }
        result.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }

    pub async fn diff(
        &self,
        old: &ObjectId,
        new: &ObjectId,
    ) -> Result<Vec<Difference>, BackendError> {
        let leaves = |entries: Vec<TreeEntry>| {
            entries
                .into_iter()
                .filter(|entry| !entry.is_tree())
                .map(|entry| (entry.path.clone(), entry))
                .collect::<BTreeMap<_, _>>()
        };
        let mut previous = leaves(self.tree(old, true).await?);
        let current = leaves(self.tree(new, true).await?);
        let mut differences = Vec::new();
        for (path, new) in current {
            let old = previous.remove(&path);
            if old.as_ref() != Some(&new) {
                differences.push(Difference {
                    path,
                    old,
                    new: Some(new),
                });
            }
        }
        differences.extend(previous.into_iter().map(|(path, old)| Difference {
            path,
            old: Some(old),
            new: None,
        }));
        differences.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(differences)
    }
}

/// This key is for warnings only. Original path bytes are never changed.
pub fn collisions(entries: &[TreeEntry]) -> Vec<Vec<Vec<u8>>> {
    let mut groups: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for entry in entries {
        if let Ok(path) = std::str::from_utf8(&entry.path) {
            let key = path.nfc().flat_map(char::to_lowercase).collect::<String>();
            groups.entry(key).or_default().push(entry.path.clone());
        }
    }
    groups
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect()
}

pub fn validate_path(path: &[u8]) -> Result<(), BackendError> {
    if path.len() > MAX_PATH_BYTES
        || path.contains(&0)
        || path
            .split(|byte| *byte == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return Err(invalid(
            "path",
            "Expected a non-empty repository-relative path",
        ));
    }
    Ok(())
}

fn invalid(field: &str, reason: &str) -> BackendError {
    BackendError::InvalidRequest {
        field: field.to_owned(),
        reason: reason.to_owned(),
    }
}
