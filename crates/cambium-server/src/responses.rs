//! Successful operation bodies shared by the service and the OpenAPI generator.
use cambium_protocol::{
    Actor, BranchId, CompactionId, IdentityId, IdentityKind, ObjectId, RepositoryId, Scope,
    TokenId, Warning,
};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Path {
    Path(String),
    PathB64(String),
}

#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Contents {
    Contents(String),
    ContentsB64(String),
}

fn warning_items(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type":"array","items":{"$ref":"#/components/schemas/Warning"}})
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RepositoryBody {
    pub repository_id: RepositoryId,
    pub name: String,
    pub default_branch: String,
    pub state: String,
    pub created_at: String,
    pub created_by: Actor,
    pub git_url: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RepositoryList {
    pub repositories: Vec<RepositoryBody>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub next_page_token: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DeletedRepository {
    pub deleted: bool,
    pub repository_id: RepositoryId,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IdentityBody {
    pub identity_id: IdentityId,
    pub kind: IdentityKind,
    pub name: String,
    pub created_at: String,
    pub created_by: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "IdentityId")]
    pub owner_id: Option<IdentityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub disabled_at: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TokenBody {
    pub token_id: TokenId,
    pub identity_id: IdentityId,
    pub kind: IdentityKind,
    pub scope: Scope,
    pub expires_at: String,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Vec<RepositoryId>")]
    pub repositories: Option<Vec<RepositoryId>>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RevokedToken {
    pub revoked: bool,
    pub token_id: TokenId,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DisabledIdentity {
    pub disabled: bool,
    pub identity_id: IdentityId,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BranchBody {
    pub branch_id: BranchId,
    pub repository_id: RepositoryId,
    pub name: String,
    pub state: String,
    pub created_at: String,
    pub created_by: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub last_activity_at: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BranchList {
    pub branches: Vec<BranchBody>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub next_page_token: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ChangesetBody {
    pub commit_id: ObjectId,
    pub tree_id: ObjectId,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ObjectId")]
    pub parent_commit_id: Option<ObjectId>,
    pub provenance_recorded: bool,
    pub path_collision_check_complete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "warning_items")]
    pub warnings: Vec<Warning>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ProvenanceBody {
    pub commit_id: ObjectId,
    pub recorded_at: String,
    pub recorded_by: Actor,
    pub claims: Claims,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Claims {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub session_id: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Entry {
    #[serde(flatten)]
    pub path: Path,
    pub object_id: ObjectId,
    pub mode: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FileBody {
    #[serde(flatten)]
    pub entry: Entry,
    pub revision: ObjectId,
    #[serde(flatten)]
    pub contents: Contents,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TreeBody {
    pub entries: Vec<Entry>,
    pub revision: ObjectId,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub next_page_token: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "warning_items")]
    pub warnings: Vec<Warning>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DiffEntry {
    #[serde(flatten)]
    pub path: Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Entry")]
    pub old: Option<Entry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Entry")]
    pub new: Option<Entry>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DiffBody {
    pub entries: Vec<DiffEntry>,
    pub from: ObjectId,
    pub to: ObjectId,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub next_page_token: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
pub(crate) struct Conflict {
    #[serde(rename = "ref")]
    pub ref_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ObjectId")]
    pub old_value: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub rescue_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "ObjectId")]
    pub active_value: Option<ObjectId>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CompactionBody {
    pub compaction_id: CompactionId,
    pub repository_id: RepositoryId,
    pub stage: String,
    pub created_at: String,
    pub updated_at: String,
    pub old_generation: i64,
    pub new_generation: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub completed_at: Option<String>,
    pub conflict_count: usize,
    pub conflicts: Vec<Conflict>,
    pub conflicts_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub next_page_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub last_error: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ReclamationBody {
    pub repository_id: RepositoryId,
    pub active_generation: i64,
    pub fenced: bool,
    pub unreclaimed_expiries: i64,
    pub overdue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub oldest_expiry_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "CompactionBody")]
    pub compaction: Option<CompactionBody>,
}

fn schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    json!(generator.into_root_schema_for::<T>())
}

pub(crate) fn schema_for_operation(name: &str) -> Value {
    match name {
        "create_repository" | "get_repository" => schema::<RepositoryBody>(),
        "list_repositories" => schema::<RepositoryList>(),
        "delete_repository" => schema::<DeletedRepository>(),
        "create_identity" | "get_identity" => schema::<IdentityBody>(),
        "disable_identity" => schema::<DisabledIdentity>(),
        "revoke_token" => schema::<RevokedToken>(),
        "mint_token" => schema::<TokenBody>(),
        "create_ephemeral_branch" | "get_branch" | "extend_branch" | "promote_branch" => {
            schema::<BranchBody>()
        }
        "list_branches" => schema::<BranchList>(),
        "write_changeset" => schema::<ChangesetBody>(),
        "get_commit_provenance" => schema::<ProvenanceBody>(),
        "read_file" => schema::<FileBody>(),
        "list_tree" => schema::<TreeBody>(),
        "diff" => schema::<DiffBody>(),
        "get_reclamation" => schema::<ReclamationBody>(),
        "request_compaction" => schema::<CompactionBody>(),
        _ => panic!("missing success body for {name}"),
    }
}

impl Path {
    pub(crate) fn encode(bytes: &[u8]) -> Self {
        match cambium_protocol::bytes::encode_pair(bytes, false) {
            (Some(text), _) => Self::Path(text),
            (_, Some(encoded)) => Self::PathB64(encoded),
            _ => unreachable!("encode_pair always emits one member"),
        }
    }
}

impl Contents {
    pub(crate) fn encode(bytes: &[u8], always_base64: bool) -> Self {
        match cambium_protocol::bytes::encode_pair(bytes, always_base64) {
            (Some(text), _) => Self::Contents(text),
            (_, Some(encoded)) => Self::ContentsB64(encoded),
            _ => unreachable!("encode_pair always emits one member"),
        }
    }
}
