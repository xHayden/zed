use crate::{
    repository::{GitRepository, RevisionContent, StackReviewContentRequest},
    status::TreeDiffStatus,
};
use anyhow::{Context as _, Result, bail};
use imara_diff::{Algorithm, Diff, InternedInput, sources::lines};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const REVIEW_STATE_SCHEMA_VERSION: u32 = 1;
const COMMENT_SCHEMA_VERSION: u32 = 1;
const CURRENT_MANIFEST_SCHEMA_VERSION: u32 = 1;
const PRESENTATION_STATE_SCHEMA_VERSION: u32 = 1;
pub const STACK_REVIEW_REVIEW_BINDING_KEY: &str = "review";
const STACK_REVIEW_COMMENT_BINDING_PREFIX: &str = "comment:";

pub fn stack_review_storage_key(base_oid: &str, head_oid: &str) -> String {
    format!("{base_oid}-{head_oid}")
        .replace(|character: char| !character.is_ascii_alphanumeric(), "_")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewCommentAuthor {
    pub name: String,
    #[serde(default)]
    pub login: Option<String>,
}

impl Default for StackReviewCommentAuthor {
    fn default() -> Self {
        Self {
            name: "You".to_owned(),
            login: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackReviewCommentSource {
    #[default]
    LocalHuman,
    LocalAgent,
    Github,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewComment {
    pub id: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
    pub path: String,
    pub start_row: u32,
    pub start_column: u32,
    pub end_row: u32,
    pub end_column: u32,
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub author: StackReviewCommentAuthor,
    #[serde(default)]
    pub source: StackReviewCommentSource,
    #[serde(default)]
    pub reply_to: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_record_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackReviewCommentSide {
    Left,
    Right,
    TopLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackReviewGitHubCommentKind {
    Inline,
    Review,
    Conversation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewGitHubCommentIdentity {
    pub pull_request_number: u32,
    pub github_id: String,
    pub url: String,
    pub kind: StackReviewGitHubCommentKind,
    #[serde(default)]
    pub commit_oid: Option<String>,
    #[serde(default)]
    pub original_commit_oid: Option<String>,
    #[serde(default)]
    pub original_line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewCommentRecord {
    pub schema_version: u32,
    pub id: String,
    pub base_oid: String,
    pub head_oid: String,
    #[serde(default)]
    pub path: Option<String>,
    pub side: StackReviewCommentSide,
    #[serde(default)]
    pub start_row: Option<u32>,
    #[serde(default)]
    pub start_column: Option<u32>,
    #[serde(default)]
    pub end_row: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
    pub body: String,
    pub author: StackReviewCommentAuthor,
    pub source: StackReviewCommentSource,
    #[serde(default)]
    pub reply_to: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub outdated: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub local_resolution: Option<bool>,
    #[serde(default)]
    pub github: Option<StackReviewGitHubCommentIdentity>,
}

impl StackReviewCommentRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new_inline(
        id: String,
        base_oid: String,
        head_oid: String,
        path: String,
        start_row: u32,
        start_column: u32,
        end_row: u32,
        end_column: u32,
        body: String,
        author: StackReviewCommentAuthor,
        source: StackReviewCommentSource,
        reply_to: Option<String>,
        timestamp: String,
    ) -> Self {
        Self {
            schema_version: COMMENT_SCHEMA_VERSION,
            id,
            base_oid,
            head_oid,
            path: Some(path),
            side: StackReviewCommentSide::Right,
            start_row: Some(start_row),
            start_column: Some(start_column),
            end_row: Some(end_row),
            end_column: Some(end_column),
            body,
            author,
            source,
            reply_to,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            outdated: false,
            resolved: false,
            local_resolution: None,
            github: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_github_inline(
        id: String,
        base_oid: String,
        head_oid: String,
        path: String,
        side: StackReviewCommentSide,
        row: u32,
        body: String,
        author: StackReviewCommentAuthor,
        reply_to: Option<String>,
        timestamp: String,
        github: StackReviewGitHubCommentIdentity,
        outdated: bool,
    ) -> Self {
        Self {
            schema_version: COMMENT_SCHEMA_VERSION,
            id,
            base_oid,
            head_oid,
            path: Some(path),
            side,
            start_row: Some(row),
            start_column: Some(0),
            end_row: Some(row),
            end_column: Some(0),
            body,
            author,
            source: StackReviewCommentSource::Github,
            reply_to,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            outdated,
            resolved: false,
            local_resolution: None,
            github: Some(github),
        }
    }

    pub fn new_github_top_level(
        id: String,
        base_oid: String,
        head_oid: String,
        body: String,
        author: StackReviewCommentAuthor,
        timestamp: String,
        github: StackReviewGitHubCommentIdentity,
    ) -> Self {
        Self {
            schema_version: COMMENT_SCHEMA_VERSION,
            id,
            base_oid,
            head_oid,
            path: None,
            side: StackReviewCommentSide::TopLevel,
            start_row: None,
            start_column: None,
            end_row: None,
            end_column: None,
            body,
            author,
            source: StackReviewCommentSource::Github,
            reply_to: None,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            outdated: false,
            resolved: false,
            local_resolution: None,
            github: Some(github),
        }
    }

    pub fn from_json(contents: &str, base_oid: &str, head_oid: &str) -> Result<Self> {
        let record: Self = serde_json::from_str(contents)?;
        record.validate(base_oid, head_oid)?;
        Ok(record)
    }

    pub fn to_json(&self) -> Result<String> {
        self.validate(&self.base_oid, &self.head_oid)?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self, base_oid: &str, head_oid: &str) -> Result<()> {
        if self.schema_version != COMMENT_SCHEMA_VERSION {
            bail!("unsupported comment schema {}", self.schema_version);
        }
        if self.id.is_empty() || self.id.contains(['/', '\\']) {
            bail!("comment id is empty or contains a path separator");
        }
        if self.source != StackReviewCommentSource::Github
            && uuid::Uuid::parse_str(&self.id).is_err()
        {
            bail!("local comment id must be a UUID");
        }
        if self.base_oid != base_oid || self.head_oid != head_oid {
            bail!("comment does not match the selected Git snapshot");
        }
        if self.body.trim().is_empty() {
            bail!("comment body is empty");
        }
        if self.reply_to.as_deref() == Some(self.id.as_str()) {
            bail!("comment cannot reply to itself");
        }
        match self.side {
            StackReviewCommentSide::Left | StackReviewCommentSide::Right => {
                if self.path.as_deref().is_none_or(str::is_empty)
                    || self.start_row.is_none()
                    || self.start_column.is_none()
                    || self.end_row.is_none()
                    || self.end_column.is_none()
                {
                    bail!("inline comment is missing its path or range");
                }
            }
            StackReviewCommentSide::TopLevel => {
                if self.path.is_some()
                    || self.start_row.is_some()
                    || self.start_column.is_some()
                    || self.end_row.is_some()
                    || self.end_column.is_some()
                {
                    bail!("top-level comment cannot contain an inline range");
                }
            }
        }
        if self.source == StackReviewCommentSource::Github && self.github.is_none() {
            bail!("GitHub comment is missing source identity");
        }
        if self.source != StackReviewCommentSource::Github && self.github.is_some() {
            bail!("local comment cannot contain GitHub source identity");
        }
        Ok(())
    }

    pub fn is_writable(&self) -> bool {
        self.source != StackReviewCommentSource::Github
    }

    pub fn is_resolved(&self) -> bool {
        self.local_resolution.unwrap_or(self.resolved)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StackReviewCommentClass {
    Inline,
    TopLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentThreadPartition {
    pub storage_key: String,
    pub base_oid: String,
    pub head_oid: String,
    pub path: Option<String>,
    pub side: StackReviewCommentSide,
    pub class: StackReviewCommentClass,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CommentThreadKey {
    Normal {
        partition: CommentThreadPartition,
        root_record_id: String,
    },
    MissingParent {
        partition: CommentThreadPartition,
        missing_parent_id: String,
    },
    BoundaryViolation {
        partition: CommentThreadPartition,
        foreign_parent_id: String,
    },
    Cycle {
        partition: CommentThreadPartition,
        canonical_member_id: String,
    },
}

impl CommentThreadKey {
    pub fn to_stable_string(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_stable_string(serialized: &str) -> Result<Self> {
        Ok(serde_json::from_str(serialized)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StackReviewPresentationState {
    schema_version: u32,
    pub base_oid: String,
    pub head_oid: String,
    #[serde(default)]
    stashed_thread_roots: BTreeSet<String>,
    #[serde(default)]
    ai_thread_bindings: BTreeMap<String, String>,
}

impl StackReviewPresentationState {
    pub fn new(base_oid: impl Into<String>, head_oid: impl Into<String>) -> Self {
        Self {
            schema_version: PRESENTATION_STATE_SCHEMA_VERSION,
            base_oid: base_oid.into(),
            head_oid: head_oid.into(),
            stashed_thread_roots: BTreeSet::new(),
            ai_thread_bindings: BTreeMap::new(),
        }
    }

    pub fn from_json(contents: &str, base_oid: &str, head_oid: &str) -> Result<Self> {
        let state: Self = serde_json::from_str(contents)?;
        state.validate(base_oid, head_oid)?;
        Ok(state)
    }

    pub fn to_json(&self) -> Result<String> {
        self.validate(&self.base_oid, &self.head_oid)?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    fn validate(&self, base_oid: &str, head_oid: &str) -> Result<()> {
        if self.schema_version != PRESENTATION_STATE_SCHEMA_VERSION {
            bail!(
                "unsupported presentation-state schema {}",
                self.schema_version
            );
        }
        if self.base_oid != base_oid || self.head_oid != head_oid {
            bail!("presentation state does not match the selected Git snapshot");
        }
        for serialized_root in &self.stashed_thread_roots {
            validate_serialized_comment_thread_key(serialized_root, base_oid, head_oid)
                .context("invalid stashed comment thread root")?;
        }
        for (binding_key, session_id) in &self.ai_thread_bindings {
            validate_ai_thread_session_id(session_id)?;
            validate_ai_thread_binding_key(binding_key, base_oid, head_oid)?;
        }
        Ok(())
    }

    pub fn stash_root(&mut self, root: &CommentThreadKey) -> Result<bool> {
        let root = self.serialized_root(root)?;
        Ok(self.stashed_thread_roots.insert(root))
    }

    pub fn restore_root(&mut self, root: &CommentThreadKey) -> Result<bool> {
        let root = self.serialized_root(root)?;
        Ok(self.stashed_thread_roots.remove(&root))
    }

    pub fn is_stashed(&self, root: &CommentThreadKey) -> Result<bool> {
        let root = self.serialized_root(root)?;
        Ok(self.stashed_thread_roots.contains(&root))
    }

    fn serialized_root(&self, root: &CommentThreadKey) -> Result<String> {
        let root = root.to_stable_string()?;
        validate_serialized_comment_thread_key(&root, &self.base_oid, &self.head_oid)?;
        Ok(root)
    }

    pub fn stashed_roots(&self) -> Result<Vec<CommentThreadKey>> {
        self.stashed_thread_roots
            .iter()
            .map(|root| {
                validate_serialized_comment_thread_key(root, &self.base_oid, &self.head_oid)
            })
            .collect()
    }

    pub fn bind_thread(
        &mut self,
        binding_key: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Option<String>> {
        let binding_key = binding_key.into();
        let session_id = session_id.into();
        validate_ai_thread_binding_key(&binding_key, &self.base_oid, &self.head_oid)?;
        validate_ai_thread_session_id(&session_id)?;
        Ok(self.ai_thread_bindings.insert(binding_key, session_id))
    }

    pub fn unbind_thread(&mut self, binding_key: &str) -> Result<Option<String>> {
        validate_ai_thread_binding_key(binding_key, &self.base_oid, &self.head_oid)?;
        Ok(self.ai_thread_bindings.remove(binding_key))
    }

    pub fn binding(&self, binding_key: &str) -> Result<Option<&str>> {
        validate_ai_thread_binding_key(binding_key, &self.base_oid, &self.head_oid)?;
        Ok(self.ai_thread_bindings.get(binding_key).map(String::as_str))
    }
}

fn validate_serialized_comment_thread_key(
    serialized: &str,
    base_oid: &str,
    head_oid: &str,
) -> Result<CommentThreadKey> {
    let key = CommentThreadKey::from_stable_string(serialized)?;
    if key.to_stable_string()? != serialized {
        bail!("comment thread key is not canonical");
    }
    let (partition, identifier) = match &key {
        CommentThreadKey::Normal {
            partition,
            root_record_id,
        } => (partition, root_record_id),
        CommentThreadKey::MissingParent {
            partition,
            missing_parent_id,
        } => (partition, missing_parent_id),
        CommentThreadKey::BoundaryViolation {
            partition,
            foreign_parent_id,
        } => (partition, foreign_parent_id),
        CommentThreadKey::Cycle {
            partition,
            canonical_member_id,
        } => (partition, canonical_member_id),
    };
    if identifier.trim().is_empty() {
        bail!("comment thread key identifier is empty");
    }
    if partition.storage_key.trim().is_empty() {
        bail!("comment thread partition storage key is empty");
    }
    if partition.base_oid != base_oid || partition.head_oid != head_oid {
        bail!("comment thread key does not match the snapshot");
    }
    if partition.storage_key != stack_review_storage_key(base_oid, head_oid) {
        bail!("comment thread key does not match the storage key");
    }
    match (partition.side, partition.class, partition.path.as_deref()) {
        (
            StackReviewCommentSide::Left | StackReviewCommentSide::Right,
            StackReviewCommentClass::Inline,
            Some(path),
        ) if !path.trim().is_empty() => {}
        (StackReviewCommentSide::TopLevel, StackReviewCommentClass::TopLevel, None) => {}
        _ => bail!("invalid comment thread partition"),
    }
    Ok(key)
}

fn validate_ai_thread_binding_key(binding_key: &str, base_oid: &str, head_oid: &str) -> Result<()> {
    if binding_key == STACK_REVIEW_REVIEW_BINDING_KEY {
        return Ok(());
    }
    let serialized_key = binding_key
        .strip_prefix(STACK_REVIEW_COMMENT_BINDING_PREFIX)
        .context("invalid AI thread binding key")?;
    validate_serialized_comment_thread_key(serialized_key, base_oid, head_oid)
        .context("invalid AI thread binding key")?;
    Ok(())
}

fn validate_ai_thread_session_id(session_id: &str) -> Result<()> {
    if session_id.trim().is_empty() {
        bail!("AI thread session ID is empty");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentThread {
    pub key: CommentThreadKey,
    pub placement_record_id: String,
    pub member_record_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CommentThreadIndex {
    thread_key_by_record_id: HashMap<String, CommentThreadKey>,
    threads: Vec<CommentThread>,
}

impl CommentThreadIndex {
    pub fn new<'a>(
        storage_key: impl Into<String>,
        records: impl IntoIterator<Item = &'a StackReviewCommentRecord>,
    ) -> Result<Self> {
        let storage_key = storage_key.into();
        let mut records_by_id = BTreeMap::new();
        for record in records {
            if records_by_id.insert(record.id.clone(), record).is_some() {
                bail!("duplicate Stack Review comment record id {:?}", record.id);
            }
        }

        let mut thread_key_by_record_id = HashMap::new();
        for record in records_by_id.values() {
            let partition = comment_thread_partition(&storage_key, record);
            let mut root = *record;
            let mut ancestry: Vec<&str> = Vec::new();
            let mut ancestry_positions: HashMap<&str, usize> = HashMap::new();
            let key = loop {
                if let Some(cycle_start) = ancestry_positions.get(root.id.as_str()) {
                    let Some(cycle_members) = ancestry.get(*cycle_start..) else {
                        bail!("invalid Stack Review comment cycle position");
                    };
                    let Some(canonical_member_id) = cycle_members.iter().min() else {
                        bail!("empty Stack Review comment cycle");
                    };
                    break CommentThreadKey::Cycle {
                        partition,
                        canonical_member_id: (*canonical_member_id).to_owned(),
                    };
                }
                ancestry_positions.insert(root.id.as_str(), ancestry.len());
                ancestry.push(root.id.as_str());
                let Some(parent_id) = root.reply_to.as_deref() else {
                    break CommentThreadKey::Normal {
                        partition,
                        root_record_id: root.id.clone(),
                    };
                };
                let Some(parent) = records_by_id.get(parent_id) else {
                    break CommentThreadKey::MissingParent {
                        partition,
                        missing_parent_id: parent_id.to_owned(),
                    };
                };
                if comment_thread_partition(&storage_key, parent) != partition {
                    break CommentThreadKey::BoundaryViolation {
                        partition,
                        foreign_parent_id: parent_id.to_owned(),
                    };
                }
                root = parent;
            };
            thread_key_by_record_id.insert(record.id.clone(), key);
        }

        let mut member_ids_by_key = BTreeMap::<CommentThreadKey, Vec<String>>::new();
        for (record_id, key) in &thread_key_by_record_id {
            member_ids_by_key
                .entry(key.clone())
                .or_default()
                .push(record_id.clone());
        }
        let mut threads = Vec::with_capacity(member_ids_by_key.len());
        for (key, member_ids) in member_ids_by_key {
            let placement_record_id = match &key {
                CommentThreadKey::Normal { root_record_id, .. } => root_record_id.clone(),
                _ => {
                    let Some(record_id) = member_ids
                        .iter()
                        .min_by(|left, right| {
                            compare_comment_record_ids(&records_by_id, left, right)
                        })
                        .cloned()
                    else {
                        continue;
                    };
                    record_id
                }
            };
            let member_record_ids =
                ordered_comment_thread_members(&records_by_id, &member_ids, &key);
            threads.push(CommentThread {
                key,
                placement_record_id,
                member_record_ids,
            });
        }
        threads.sort_by(|left, right| {
            compare_comment_record_ids(
                &records_by_id,
                &left.placement_record_id,
                &right.placement_record_id,
            )
            .then_with(|| left.key.cmp(&right.key))
        });

        Ok(Self {
            thread_key_by_record_id,
            threads,
        })
    }

    pub fn thread_key_for(&self, record_id: &str) -> Option<&CommentThreadKey> {
        self.thread_key_by_record_id.get(record_id)
    }

    pub fn thread(&self, key: &CommentThreadKey) -> Option<&CommentThread> {
        self.threads.iter().find(|thread| &thread.key == key)
    }

    pub fn threads(&self) -> &[CommentThread] {
        &self.threads
    }
}

fn comment_thread_partition(
    storage_key: &str,
    record: &StackReviewCommentRecord,
) -> CommentThreadPartition {
    CommentThreadPartition {
        storage_key: storage_key.to_owned(),
        base_oid: record.base_oid.clone(),
        head_oid: record.head_oid.clone(),
        path: record.path.clone(),
        side: record.side,
        class: if record.side == StackReviewCommentSide::TopLevel {
            StackReviewCommentClass::TopLevel
        } else {
            StackReviewCommentClass::Inline
        },
    }
}

fn comment_record_timestamp_nanos(record: &StackReviewCommentRecord) -> i128 {
    time::OffsetDateTime::parse(
        &record.created_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|timestamp| timestamp.unix_timestamp_nanos())
    .unwrap_or(i128::MIN)
}

fn compare_comment_record_ids(
    records_by_id: &BTreeMap<String, &StackReviewCommentRecord>,
    left_id: &str,
    right_id: &str,
) -> std::cmp::Ordering {
    records_by_id
        .get(left_id)
        .map(|record| comment_record_timestamp_nanos(record))
        .unwrap_or(i128::MIN)
        .cmp(
            &records_by_id
                .get(right_id)
                .map(|record| comment_record_timestamp_nanos(record))
                .unwrap_or(i128::MIN),
        )
        .then_with(|| left_id.cmp(right_id))
}

fn ordered_comment_thread_members(
    records_by_id: &BTreeMap<String, &StackReviewCommentRecord>,
    member_ids: &[String],
    key: &CommentThreadKey,
) -> Vec<String> {
    let member_ids = member_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut children_by_parent = HashMap::<&str, Vec<&str>>::new();
    for member_id in &member_ids {
        let Some(record) = records_by_id.get(*member_id) else {
            continue;
        };
        if let Some(parent_id) = record.reply_to.as_deref()
            && member_ids.contains(parent_id)
        {
            children_by_parent
                .entry(parent_id)
                .or_default()
                .push(member_id);
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_by(|left, right| compare_comment_record_ids(records_by_id, left, right));
    }

    let mut ordered = Vec::with_capacity(member_ids.len());
    let mut roots = member_ids
        .iter()
        .filter(|record_id| {
            records_by_id
                .get(**record_id)
                .and_then(|record| record.reply_to.as_deref())
                .is_none_or(|parent_id| !member_ids.contains(parent_id))
        })
        .copied()
        .collect::<Vec<_>>();
    if roots.is_empty()
        && let CommentThreadKey::Cycle {
            canonical_member_id,
            ..
        } = key
    {
        roots.push(canonical_member_id);
    }
    roots.sort_by(|left, right| compare_comment_record_ids(records_by_id, left, right));
    let mut pending = roots.into_iter().rev().collect::<Vec<_>>();
    let mut visited = HashSet::new();
    while let Some(record_id) = pending.pop() {
        if !visited.insert(record_id) {
            continue;
        }
        ordered.push(record_id.to_owned());
        if let Some(children) = children_by_parent.get(record_id) {
            pending.extend(children.iter().rev().copied());
        }
    }
    ordered
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewCurrentManifest {
    pub schema_version: u32,
    pub repository: String,
    pub base_oid: String,
    pub head_oid: String,
    pub review_state_path: String,
    pub comments_path: String,
    pub github_comments_path: String,
    pub layers: Vec<u32>,
}

impl StackReviewCurrentManifest {
    pub fn new(
        repository: String,
        base_oid: String,
        head_oid: String,
        review_state_path: String,
        comments_path: String,
        github_comments_path: String,
        layers: Vec<u32>,
    ) -> Self {
        Self {
            schema_version: CURRENT_MANIFEST_SCHEMA_VERSION,
            repository,
            base_oid,
            head_oid,
            review_state_path,
            comments_path,
            github_comments_path,
            layers,
        }
    }

    pub fn from_json(contents: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(contents)?;
        if manifest.schema_version != CURRENT_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported current-manifest schema {}",
                manifest.schema_version
            );
        }
        Ok(manifest)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackReviewState {
    schema_version: u32,
    pub base_oid: String,
    pub head_oid: String,
    #[serde(default, skip_serializing)]
    reviewed_files: BTreeSet<String>,
    #[serde(default)]
    reviewed_file_fingerprints: BTreeMap<String, String>,
    #[serde(default)]
    comments: Vec<StackReviewComment>,
}

impl StackReviewState {
    pub fn new(base_oid: impl Into<String>, head_oid: impl Into<String>) -> Self {
        Self {
            schema_version: REVIEW_STATE_SCHEMA_VERSION,
            base_oid: base_oid.into(),
            head_oid: head_oid.into(),
            reviewed_files: BTreeSet::new(),
            reviewed_file_fingerprints: BTreeMap::new(),
            comments: Vec::new(),
        }
    }

    pub fn from_json(contents: &str) -> Result<Self> {
        let mut state: Self = serde_json::from_str(contents)?;
        if state.schema_version != REVIEW_STATE_SCHEMA_VERSION {
            bail!("unsupported review-state schema {}", state.schema_version);
        }
        let mut seen_ids = BTreeSet::new();
        let mut next_id = state
            .comments
            .iter()
            .map(|comment| comment.id)
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        for comment in &mut state.comments {
            if !seen_ids.insert(comment.id) {
                comment.id = next_id;
                seen_ids.insert(next_id);
                next_id = next_id.saturating_add(1);
            }
        }
        Ok(state)
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn matches_snapshot(&self, base_oid: &str, head_oid: &str) -> bool {
        self.base_oid == base_oid && self.head_oid == head_oid
    }

    pub fn is_file_reviewed(&self, path: &str, fingerprint: &str) -> bool {
        self.reviewed_file_fingerprints
            .get(path)
            .is_some_and(|reviewed_fingerprint| reviewed_fingerprint == fingerprint)
    }

    pub fn set_file_reviewed(
        &mut self,
        path: impl Into<String>,
        fingerprint: impl Into<String>,
        reviewed: bool,
    ) {
        let path = path.into();
        if reviewed {
            self.reviewed_file_fingerprints
                .insert(path, fingerprint.into());
        } else {
            self.reviewed_file_fingerprints.remove(&path);
        }
    }

    pub fn reviewed_file_count(&self) -> usize {
        self.reviewed_file_fingerprints.len()
    }

    pub fn comments(&self) -> &[StackReviewComment] {
        &self.comments
    }

    pub fn take_comments(&mut self) -> Vec<StackReviewComment> {
        std::mem::take(&mut self.comments)
    }

    pub fn set_comments(&mut self, comments: Vec<StackReviewComment>) {
        self.comments = comments;
    }

    pub fn reconcile_rendered_comments_for_path(
        &mut self,
        path: &str,
        rendered_comment_ids: &HashSet<usize>,
        comments: Vec<StackReviewComment>,
    ) {
        self.comments
            .retain(|comment| comment.path != path || !rendered_comment_ids.contains(&comment.id));
        self.comments.extend(comments);
        self.comments.sort_by_key(|comment| comment.id);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstParentCommit {
    pub oid: String,
    pub author_timestamp: i64,
    pub is_merge: bool,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedStackBranch {
    pub branch: String,
    pub oid: String,
    pub pull_request_number: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedStackLayer {
    pub base: ResolvedStackBranch,
    pub head: ResolvedStackBranch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackSnapshot {
    pub number: Option<u32>,
    pub trunk: ResolvedStackBranch,
    pub layers: Vec<ResolvedStackLayer>,
}

impl StackSnapshot {
    pub fn boundary(&self, index: usize) -> Option<&ResolvedStackBranch> {
        if index == 0 {
            Some(&self.trunk)
        } else {
            self.layers
                .get(index.checked_sub(1)?)
                .map(|layer| &layer.head)
        }
    }

    pub fn refs_between(&self, from: usize, to: usize) -> Option<(&str, &str)> {
        if from >= to {
            return None;
        }
        Some((
            self.boundary(from)?.oid.as_str(),
            self.boundary(to)?.oid.as_str(),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackReviewFileStatus {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackReviewFileProvenance {
    Direct,
    Merge,
    Mixed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackReviewContentKind {
    Text,
    Binary,
    NonBlob,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StackReviewFileDiff {
    pub path: String,
    pub status: StackReviewFileStatus,
    pub old_content: Option<RevisionContent>,
    pub new_content: Option<RevisionContent>,
    pub provenance: StackReviewFileProvenance,
    pub content_kind: StackReviewContentKind,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
}

fn stack_review_line_counts(old_text: &str, new_text: &str) -> (u32, u32) {
    let input = InternedInput::new(lines(old_text), lines(new_text));
    let mut additions = 0;
    let mut deletions = 0;
    for hunk in Diff::compute(Algorithm::Histogram, &input).hunks() {
        additions += hunk.after.len() as u32;
        deletions += hunk.before.len() as u32;
    }
    (additions, deletions)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StackReviewProvenanceCounts {
    pub direct: usize,
    pub merge: usize,
    pub mixed: usize,
    pub unknown: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StackReviewDiff {
    pub base_ref: String,
    pub head_ref: String,
    pub files: Vec<StackReviewFileDiff>,
}

impl StackReviewDiff {
    pub fn file(&self, path: &str) -> Option<&StackReviewFileDiff> {
        self.files.iter().find(|file| file.path == path)
    }

    pub fn provenance_counts(&self) -> StackReviewProvenanceCounts {
        let mut counts = StackReviewProvenanceCounts::default();
        for file in &self.files {
            match file.provenance {
                StackReviewFileProvenance::Direct => counts.direct += 1,
                StackReviewFileProvenance::Merge => counts.merge += 1,
                StackReviewFileProvenance::Mixed => counts.mixed += 1,
                StackReviewFileProvenance::Unknown => counts.unknown += 1,
            }
        }
        counts
    }
}

pub async fn load_stack_diff(
    repository: &dyn GitRepository,
    base_ref: &str,
    head_ref: &str,
) -> Result<StackReviewDiff> {
    let commits = repository
        .first_parent_commits(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    load_stack_diff_with_commits(repository, base_ref, head_ref, commits, None).await
}

async fn load_stack_diff_with_commits(
    repository: &dyn GitRepository,
    base_ref: &str,
    head_ref: &str,
    commits: Vec<FirstParentCommit>,
    included_paths: Option<&HashSet<String>>,
) -> Result<StackReviewDiff> {
    let tree_diff = repository
        .stack_review_diff_tree(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    let mut entries = tree_diff.entries.into_iter().collect::<Vec<_>>();
    if let Some(included_paths) = included_paths {
        entries.retain(|(path, _)| included_paths.contains(path.as_unix_str()));
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut provenance_by_path = HashMap::<String, (bool, bool)>::new();
    for commit in commits {
        for path in commit.paths {
            let provenance = provenance_by_path.entry(path).or_default();
            if commit.is_merge {
                provenance.1 = true;
            } else {
                provenance.0 = true;
            }
        }
    }
    let unfiltered_file_count = entries.len();
    entries.retain(|(path, _)| {
        !matches!(
            provenance_by_path.get(path.as_unix_str()).copied(),
            Some((false, true))
        )
    });
    log::debug!(
        "[STACK_REVIEW_DEBUG] excluded {} merge-only files from {base_ref}..{head_ref}",
        unfiltered_file_count.saturating_sub(entries.len())
    );

    let requests = entries
        .iter()
        .map(|(path, status)| StackReviewContentRequest {
            path: path.clone(),
            old_oid: match status {
                TreeDiffStatus::Added => None,
                TreeDiffStatus::Modified { old } | TreeDiffStatus::Deleted { old } => Some(*old),
            },
            include_new: !matches!(status, TreeDiffStatus::Deleted { .. }),
        })
        .collect();
    let contents = repository
        .stack_review_load_contents(head_ref.to_owned(), requests)
        .await?;
    anyhow::ensure!(
        contents.len() == entries.len(),
        "stack content count mismatch"
    );

    let mut files = Vec::with_capacity(entries.len());
    for ((path, status), content) in entries.into_iter().zip(contents) {
        let file_status = match status {
            TreeDiffStatus::Added => StackReviewFileStatus::Added,
            TreeDiffStatus::Modified { .. } => StackReviewFileStatus::Modified,
            TreeDiffStatus::Deleted { .. } => StackReviewFileStatus::Deleted,
        };
        let path_text = path.as_unix_str().to_owned();
        let old_content = content.old;
        let new_content = content.new;
        let content_kind = if old_content
            .iter()
            .chain(new_content.iter())
            .any(|content| matches!(content, RevisionContent::Unavailable(_)))
        {
            StackReviewContentKind::Unavailable
        } else if old_content
            .iter()
            .chain(new_content.iter())
            .any(|content| matches!(content, RevisionContent::NonBlob(_)))
        {
            StackReviewContentKind::NonBlob
        } else if old_content
            .iter()
            .chain(new_content.iter())
            .any(|content| matches!(content, RevisionContent::Binary))
        {
            StackReviewContentKind::Binary
        } else {
            StackReviewContentKind::Text
        };
        let provenance = match provenance_by_path.get(&path_text).copied() {
            Some((true, false)) => StackReviewFileProvenance::Direct,
            Some((false, true)) => StackReviewFileProvenance::Merge,
            Some((true, true)) => StackReviewFileProvenance::Mixed,
            _ => StackReviewFileProvenance::Unknown,
        };
        let line_counts = match (old_content.as_ref(), new_content.as_ref()) {
            (None, None) => None,
            (old_content, new_content) => {
                let old_text = match old_content {
                    Some(RevisionContent::Text(text)) => Some(text.as_str()),
                    None => Some(""),
                    _ => None,
                };
                let new_text = match new_content {
                    Some(RevisionContent::Text(text)) => Some(text.as_str()),
                    None => Some(""),
                    _ => None,
                };
                old_text
                    .zip(new_text)
                    .map(|(old_text, new_text)| stack_review_line_counts(old_text, new_text))
            }
        };
        files.push(StackReviewFileDiff {
            path: path_text,
            status: file_status,
            old_content,
            new_content,
            provenance,
            content_kind,
            additions: line_counts.map(|counts| counts.0),
            deletions: line_counts.map(|counts| counts.1),
        });
    }

    Ok(StackReviewDiff {
        base_ref: base_ref.to_owned(),
        head_ref: head_ref.to_owned(),
        files,
    })
}

pub async fn load_stack_diff_since(
    repository: &dyn GitRepository,
    base_ref: &str,
    head_ref: &str,
    author_timestamp: i64,
) -> Result<StackReviewDiff> {
    let commits = repository
        .first_parent_commits(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    let included_paths = commits
        .iter()
        .filter(|commit| commit.author_timestamp >= author_timestamp)
        .flat_map(|commit| commit.paths.iter().cloned())
        .collect::<HashSet<_>>();
    load_stack_diff_with_commits(
        repository,
        base_ref,
        head_ref,
        commits,
        Some(&included_paths),
    )
    .await
}

fn time_checkpoint_base(
    base_ref: &str,
    head_ref: &str,
    commits: &[FirstParentCommit],
    author_timestamp: i64,
) -> String {
    match commits
        .iter()
        .position(|commit| commit.author_timestamp >= author_timestamp)
    {
        Some(0) => base_ref.to_owned(),
        Some(index) => commits[index - 1].oid.clone(),
        None => head_ref.to_owned(),
    }
}

pub async fn resolve_stack_review_commit_boundary(
    repository: &dyn GitRepository,
    base_ref: &str,
    head_ref: &str,
    candidate: &str,
) -> Result<String> {
    let effective_base = repository
        .stack_review_merge_base(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    let candidate_oid = repository
        .stack_review_resolve_revision(candidate.to_owned())
        .await?;
    let is_direct_boundary = repository
        .is_ancestor(effective_base.clone(), candidate_oid.clone())
        .await?
        && repository
            .is_ancestor(candidate_oid.clone(), head_ref.to_owned())
            .await?;
    if is_direct_boundary {
        return Ok(candidate_oid);
    }

    let equivalent_commits = repository
        .stack_review_patch_equivalent_commits(
            effective_base,
            head_ref.to_owned(),
            candidate_oid.clone(),
        )
        .await?;
    match equivalent_commits.as_slice() {
        [equivalent] => Ok(equivalent.clone()),
        [] => anyhow::bail!(
            "commit boundary {candidate_oid} is not within the selected range and has no patch-equivalent rewritten commit"
        ),
        _ => anyhow::bail!(
            "commit boundary {candidate_oid} has multiple patch-equivalent commits in the selected range"
        ),
    }
}

pub async fn resolve_stack_review_time_checkpoint(
    repository: &dyn GitRepository,
    base_ref: &str,
    head_ref: &str,
    author_timestamp: i64,
) -> Result<String> {
    let commits = repository
        .first_parent_commits(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    Ok(time_checkpoint_base(
        base_ref,
        head_ref,
        &commits,
        author_timestamp,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackLayerAncestry {
    pub base_branch: String,
    pub head_branch: String,
    pub is_ancestor: bool,
}

pub async fn inspect_stack_ancestry(
    repository: &dyn GitRepository,
    snapshot: &StackSnapshot,
) -> Result<Vec<StackLayerAncestry>> {
    let mut ancestry = Vec::with_capacity(snapshot.layers.len());
    for layer in &snapshot.layers {
        ancestry.push(StackLayerAncestry {
            base_branch: layer.base.branch.clone(),
            head_branch: layer.head.branch.clone(),
            is_ancestor: repository
                .is_ancestor(layer.base.oid.clone(), layer.head.oid.clone())
                .await?,
        });
    }
    Ok(ancestry)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StackFile {
    pub schema_version: u32,
    #[serde(default)]
    pub repository: Option<String>,
    pub stacks: Vec<Stack>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stack {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub number: Option<u32>,
    pub trunk: BranchRef,
    pub branches: Vec<BranchRef>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BranchRef {
    pub branch: String,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestRef {
    pub number: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub merged: bool,
}

pub fn parse_stack_file(contents: &str) -> Result<StackFile> {
    let file: StackFile = serde_json::from_str(contents)?;
    if file.schema_version != SUPPORTED_SCHEMA_VERSION {
        bail!(
            "unsupported gh-stack schema version {}",
            file.schema_version
        );
    }
    Ok(file)
}

impl StackFile {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    pub fn stack_for_branch(&self, branch: &str) -> Result<&Stack> {
        let matches = self
            .stacks
            .iter()
            .filter(|stack| stack.contains(branch))
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [stack] => Ok(*stack),
            [] => bail!("branch {branch:?} is not part of a local stack"),
            matches => bail!(
                "branch {branch:?} belongs to {} local stacks",
                matches.len()
            ),
        }
    }
}

impl Stack {
    pub fn contains(&self, branch: &str) -> bool {
        self.trunk.branch == branch
            || self
                .branches
                .iter()
                .any(|candidate| candidate.branch == branch)
    }

    pub fn base_branch_for(&self, branch: &str) -> Option<&str> {
        let index = self
            .branches
            .iter()
            .position(|candidate| candidate.branch == branch)?;
        if index == 0 {
            Some(self.trunk.branch.as_str())
        } else {
            self.branches
                .get(index - 1)
                .map(|candidate| candidate.branch.as_str())
        }
    }

    pub fn resolve(&self, branch_heads: &HashMap<String, String>) -> Result<StackSnapshot> {
        let trunk = resolve_branch(&self.trunk, branch_heads)?;
        let mut base = trunk.clone();
        let mut layers = Vec::with_capacity(self.branches.len());

        for branch in &self.branches {
            let head = resolve_branch(branch, branch_heads)?;
            layers.push(ResolvedStackLayer {
                base: base.clone(),
                head: head.clone(),
            });
            base = head;
        }

        Ok(StackSnapshot {
            number: self.number,
            trunk,
            layers,
        })
    }
}

fn resolve_branch(
    branch: &BranchRef,
    branch_heads: &HashMap<String, String>,
) -> Result<ResolvedStackBranch> {
    let oid = branch_heads
        .get(&branch.branch)
        .with_context(|| format!("missing local branch {:?}", branch.branch))?;
    Ok(ResolvedStackBranch {
        branch: branch.branch.clone(),
        oid: oid.clone(),
        pull_request_number: branch
            .pull_request
            .as_ref()
            .map(|pull_request| pull_request.number),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::GitBinary;

    #[test]
    fn github_comment_identity_defaults_original_anchor_for_cached_records() {
        let record = StackReviewCommentRecord::new_github_inline(
            "github-comment".into(),
            "base".into(),
            "head".into(),
            "src/lib.rs".into(),
            StackReviewCommentSide::Right,
            4,
            "Comment".into(),
            StackReviewCommentAuthor {
                name: "Reviewer".into(),
                login: Some("reviewer".into()),
            },
            None,
            "2026-08-24T00:00:00Z".into(),
            StackReviewGitHubCommentIdentity {
                pull_request_number: 1,
                github_id: "10".into(),
                url: "https://github.test/comment".into(),
                kind: StackReviewGitHubCommentKind::Inline,
                commit_oid: Some("base".into()),
                original_commit_oid: Some("base".into()),
                original_line: Some(2),
            },
            false,
        );
        let mut json = serde_json::to_value(record).unwrap();
        let github = json
            .get_mut("github")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        github.remove("originalCommitOid");
        github.remove("originalLine");

        let restored = StackReviewCommentRecord::from_json(
            &serde_json::to_string(&json).unwrap(),
            "base",
            "head",
        )
        .unwrap();
        let github = restored.github.unwrap();
        assert_eq!(github.original_commit_oid, None);
        assert_eq!(github.original_line, None);
    }

    fn thread_record(
        id: &str,
        reply_to: Option<&str>,
        created_at: &str,
    ) -> StackReviewCommentRecord {
        StackReviewCommentRecord::new_inline(
            id.to_owned(),
            "base".into(),
            "head".into(),
            "src/lib.rs".into(),
            1,
            0,
            1,
            1,
            id.to_owned(),
            StackReviewCommentAuthor::default(),
            StackReviewCommentSource::Github,
            reply_to.map(str::to_owned),
            created_at.to_owned(),
        )
    }

    fn presentation_thread_key(root_record_id: &str) -> CommentThreadKey {
        CommentThreadKey::Normal {
            partition: CommentThreadPartition {
                storage_key: stack_review_storage_key("base", "head"),
                base_oid: "base".into(),
                head_oid: "head".into(),
                path: Some("src/lib.rs".into()),
                side: StackReviewCommentSide::Right,
                class: StackReviewCommentClass::Inline,
            },
            root_record_id: root_record_id.into(),
        }
    }

    #[test]
    fn presentation_state_round_trip_is_deterministic_and_deduplicates_roots() {
        let root_a = presentation_thread_key("a")
            .to_stable_string()
            .expect("serialize root a");
        let root_z = presentation_thread_key("z")
            .to_stable_string()
            .expect("serialize root z");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [root_z, root_a, root_a],
            "aiThreadBindings": {
                format!("comment:{root_z}"): "session-z",
                "review": "session-review",
                format!("comment:{root_a}"): "session-a"
            }
        })
        .to_string();

        let state = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect("load presentation state");
        let serialized = state.to_json().expect("serialize presentation state");
        let reopened = StackReviewPresentationState::from_json(&serialized, "base", "head")
            .expect("reopen presentation state");
        let value: serde_json::Value =
            serde_json::from_str(&serialized).expect("parse serialized presentation state");
        let mut expected_roots = vec![root_a, root_z];
        expected_roots.sort();

        assert_eq!(reopened, state);
        assert_eq!(reopened.to_json().expect("reserialize state"), serialized);
        assert_eq!(
            value["stashedThreadRoots"],
            serde_json::to_value(expected_roots).expect("serialize expected roots")
        );
    }

    #[test]
    fn presentation_state_defaults_absent_v1_collections() {
        let state = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head"
            }"#,
            "base",
            "head",
        )
        .expect("load legacy presentation state");

        assert_eq!(state, StackReviewPresentationState::new("base", "head"));
        assert!(
            state
                .stashed_roots()
                .expect("list stashed roots")
                .is_empty()
        );
        assert_eq!(
            state
                .binding(STACK_REVIEW_REVIEW_BINDING_KEY)
                .expect("review binding"),
            None
        );
    }

    #[test]
    fn presentation_state_rejects_unknown_schema_versions() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 2,
                "baseOid": "base",
                "headOid": "head"
            }"#,
            "base",
            "head",
        )
        .expect_err("unknown presentation-state schema must fail");

        assert!(
            error
                .to_string()
                .contains("unsupported presentation-state schema 2")
        );
    }

    #[test]
    fn presentation_state_rejects_unknown_fields() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head",
                "selectedContext": "stale-comment"
            }"#,
            "base",
            "head",
        )
        .expect_err("unknown presentation-state fields must fail");

        assert!(
            error
                .to_string()
                .contains("unknown field `selectedContext`")
        );
    }

    #[test]
    fn presentation_state_rejects_a_different_snapshot() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "other-base",
                "headOid": "head"
            }"#,
            "base",
            "head",
        )
        .expect_err("mismatched presentation-state snapshot must fail");

        assert!(
            error
                .to_string()
                .contains("presentation state does not match the selected Git snapshot")
        );
    }

    #[test]
    fn presentation_state_rejects_malformed_stashed_thread_keys() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head",
                "stashedThreadRoots": ["not-json"]
            }"#,
            "base",
            "head",
        )
        .expect_err("malformed stashed thread key must fail");

        assert!(
            error
                .to_string()
                .contains("invalid stashed comment thread root")
        );
    }

    #[test]
    fn presentation_state_rejects_empty_comment_thread_key_identifiers() {
        let empty_root = presentation_thread_key("")
            .to_stable_string()
            .expect("serialize empty root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [empty_root]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("empty comment thread key identifier must fail");

        assert!(format!("{error:#}").contains("comment thread key identifier is empty"));
    }

    #[test]
    fn presentation_state_rejects_comment_thread_keys_from_another_snapshot() {
        let mut foreign_root = presentation_thread_key("root");
        let CommentThreadKey::Normal { partition, .. } = &mut foreign_root else {
            panic!("expected normal thread key");
        };
        partition.base_oid = "other-base".into();
        let foreign_root = foreign_root
            .to_stable_string()
            .expect("serialize foreign root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [foreign_root]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("foreign comment thread key must fail");

        assert!(format!("{error:#}").contains("comment thread key does not match the snapshot"));
    }

    #[test]
    fn presentation_state_rejects_comment_thread_keys_from_another_storage_key() {
        let mut foreign_root = presentation_thread_key("root");
        let CommentThreadKey::Normal { partition, .. } = &mut foreign_root else {
            panic!("expected normal thread key");
        };
        partition.storage_key = "different-storage-key".into();
        let foreign_root = foreign_root
            .to_stable_string()
            .expect("serialize foreign storage root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [foreign_root]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("foreign storage key must fail");

        assert!(format!("{error:#}").contains("comment thread key does not match the storage key"));
    }

    #[test]
    fn presentation_state_rejects_empty_comment_thread_partition_keys() {
        let mut root = presentation_thread_key("root");
        let CommentThreadKey::Normal { partition, .. } = &mut root else {
            panic!("expected normal thread key");
        };
        partition.storage_key.clear();
        let root = root
            .to_stable_string()
            .expect("serialize empty-partition root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [root]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("empty comment thread partition key must fail");

        assert!(format!("{error:#}").contains("comment thread partition storage key is empty"));
    }

    #[test]
    fn presentation_state_rejects_inconsistent_comment_thread_partitions() {
        let mut root = presentation_thread_key("root");
        let CommentThreadKey::Normal { partition, .. } = &mut root else {
            panic!("expected normal thread key");
        };
        partition.class = StackReviewCommentClass::TopLevel;
        let root = root
            .to_stable_string()
            .expect("serialize inconsistent root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [root]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("inconsistent comment thread partition must fail");

        assert!(format!("{error:#}").contains("invalid comment thread partition"));
    }

    #[test]
    fn presentation_state_rejects_noncanonical_comment_thread_keys() {
        let root = presentation_thread_key("root")
            .to_stable_string()
            .expect("serialize root key");
        let contents = serde_json::json!({
            "schemaVersion": 1,
            "baseOid": "base",
            "headOid": "head",
            "stashedThreadRoots": [format!(" {root}")]
        })
        .to_string();

        let error = StackReviewPresentationState::from_json(&contents, "base", "head")
            .expect_err("noncanonical comment thread key must fail");

        assert!(format!("{error:#}").contains("comment thread key is not canonical"));
    }

    #[test]
    fn presentation_state_rejects_malformed_comment_binding_keys() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head",
                "aiThreadBindings": { "comment:not-json": "session" }
            }"#,
            "base",
            "head",
        )
        .expect_err("malformed comment binding key must fail");

        assert!(error.to_string().contains("invalid AI thread binding key"));
    }

    #[test]
    fn presentation_state_rejects_empty_ai_session_bindings() {
        let error = StackReviewPresentationState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head",
                "aiThreadBindings": { "review": "" }
            }"#,
            "base",
            "head",
        )
        .expect_err("empty AI session binding must fail");

        assert!(error.to_string().contains("AI thread session ID is empty"));
    }

    #[test]
    fn presentation_state_helpers_manage_stashes_and_thread_bindings() {
        let root = presentation_thread_key("root");
        let comment_binding_key = format!(
            "comment:{}",
            root.to_stable_string().expect("serialize comment root")
        );
        let mut state = StackReviewPresentationState::new("base", "head");

        assert!(!state.is_stashed(&root).expect("check unstashed root"));
        assert!(state.stash_root(&root).expect("stash root"));
        assert!(!state.stash_root(&root).expect("deduplicate stashed root"));
        assert!(state.is_stashed(&root).expect("check stashed root"));
        assert_eq!(
            state.stashed_roots().expect("list stashed roots"),
            std::slice::from_ref(&root)
        );
        assert!(state.restore_root(&root).expect("restore root"));
        assert!(!state.restore_root(&root).expect("root already restored"));

        assert_eq!(state.binding("review").expect("review binding"), None);
        assert_eq!(
            state
                .bind_thread("review", "review-session")
                .expect("bind review thread"),
            None
        );
        assert_eq!(
            state.binding("review").expect("bound review thread"),
            Some("review-session")
        );
        assert_eq!(
            state
                .bind_thread(&comment_binding_key, "comment-session")
                .expect("bind comment thread"),
            None
        );
        assert_eq!(
            state
                .unbind_thread(&comment_binding_key)
                .expect("unbind comment thread"),
            Some("comment-session".to_owned())
        );
    }

    #[test]
    fn presentation_state_stash_helpers_reject_foreign_snapshot_roots() {
        let mut foreign_root = presentation_thread_key("root");
        let CommentThreadKey::Normal { partition, .. } = &mut foreign_root else {
            panic!("expected normal thread key");
        };
        partition.head_oid = "other-head".into();
        let mut state = StackReviewPresentationState::new("base", "head");

        let error = state
            .stash_root(&foreign_root)
            .expect_err("foreign root must not be stashed");

        assert!(
            error
                .to_string()
                .contains("comment thread key does not match the snapshot")
        );
        assert!(
            state
                .stashed_roots()
                .expect("list stashed roots")
                .is_empty()
        );
    }

    #[test]
    fn comment_thread_index_maps_a_deep_chain_to_its_stable_root() {
        let records = [
            thread_record("grandchild", Some("child"), "2026-08-21T12:02:00Z"),
            thread_record("root", None, "2026-08-21T12:00:00Z"),
            thread_record("child", Some("root"), "2026-08-21T12:01:00Z"),
        ];

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build comment thread index");
        let expected_key = CommentThreadKey::Normal {
            partition: CommentThreadPartition {
                storage_key: "base-head".into(),
                base_oid: "base".into(),
                head_oid: "head".into(),
                path: Some("src/lib.rs".into()),
                side: StackReviewCommentSide::Right,
                class: StackReviewCommentClass::Inline,
            },
            root_record_id: "root".into(),
        };

        assert_eq!(index.thread_key_for("grandchild"), Some(&expected_key));
        assert_eq!(
            index
                .thread(&expected_key)
                .expect("normal thread")
                .member_record_ids,
            ["root", "child", "grandchild"]
        );
    }
    #[test]
    fn comment_thread_index_preserves_a_deep_normal_chain() {
        let record_ids = (0..64)
            .map(|depth| format!("node-{depth:02}"))
            .collect::<Vec<_>>();
        let records = record_ids
            .iter()
            .enumerate()
            .map(|(depth, record_id)| {
                thread_record(
                    record_id,
                    depth
                        .checked_sub(1)
                        .and_then(|parent_depth| record_ids.get(parent_depth))
                        .map(String::as_str),
                    "2026-08-21T12:00:00Z",
                )
            })
            .collect::<Vec<_>>();

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build deep normal comment thread index");
        let deepest_record_id = record_ids.last().expect("deepest record id");
        let key = index
            .thread_key_for(deepest_record_id)
            .expect("deep normal thread key");

        assert!(matches!(
            key,
            CommentThreadKey::Normal { root_record_id, .. } if root_record_id == "node-00"
        ));
        assert_eq!(
            &index
                .thread(key)
                .expect("deep normal thread")
                .member_record_ids,
            &record_ids
        );
    }

    #[test]
    fn comment_thread_index_marks_missing_parent_groups_as_degraded() {
        let records = [
            thread_record("descendant", Some("orphan"), "2026-08-21T12:02:00Z"),
            thread_record("orphan", Some("missing"), "2026-08-21T12:01:00Z"),
        ];

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build degraded comment thread index");
        let key = index
            .thread_key_for("descendant")
            .expect("missing-parent key");
        assert!(matches!(
            key,
            CommentThreadKey::MissingParent {
                missing_parent_id,
                ..
            } if missing_parent_id == "missing"
        ));
        let thread = index.thread(key).expect("missing-parent thread");
        assert_eq!(thread.placement_record_id, "orphan");
        assert_eq!(thread.member_record_ids, ["orphan", "descendant"]);
    }

    #[test]
    fn comment_thread_index_marks_cross_side_parents_as_boundary_violations() {
        let mut left_parent = thread_record("left-parent", None, "2026-08-21T12:00:00.000000001Z");
        left_parent.side = StackReviewCommentSide::Left;
        let right_child = thread_record(
            "right-child",
            Some("left-parent"),
            "2026-08-21T12:00:00.000000002Z",
        );
        let records = [left_parent, right_child];

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build boundary-qualified comment thread index");
        let child_key = index
            .thread_key_for("right-child")
            .expect("boundary-violation key");
        assert!(matches!(
            child_key,
            CommentThreadKey::BoundaryViolation {
                partition,
                foreign_parent_id,
            } if partition.side == StackReviewCommentSide::Right
                && foreign_parent_id == "left-parent"
        ));
        assert_ne!(
            child_key,
            index
                .thread_key_for("left-parent")
                .expect("normal LEFT root key")
        );
    }

    #[test]
    fn comment_thread_index_keeps_cross_boundary_descendants_in_exact_partitions() {
        let root = thread_record("root", None, "2026-08-21T12:00:00.000000001Z");
        let normal_child = thread_record(
            "normal-child",
            Some("root"),
            "2026-08-21T12:00:00.000000002Z",
        );

        let mut path_child =
            thread_record("path-child", Some("root"), "2026-08-21T12:00:00.000000003Z");
        path_child.path = Some("src/other.rs".into());
        let mut path_descendant = thread_record(
            "path-descendant",
            Some("path-child"),
            "2026-08-21T12:00:00.000000004Z",
        );
        path_descendant.path = path_child.path.clone();

        let mut snapshot_child = thread_record(
            "snapshot-child",
            Some("root"),
            "2026-08-21T12:00:00.000000005Z",
        );
        snapshot_child.base_oid = "other-base".into();
        snapshot_child.head_oid = "other-head".into();
        let mut snapshot_descendant = thread_record(
            "snapshot-descendant",
            Some("snapshot-child"),
            "2026-08-21T12:00:00.000000006Z",
        );
        snapshot_descendant.base_oid = snapshot_child.base_oid.clone();
        snapshot_descendant.head_oid = snapshot_child.head_oid.clone();

        let mut side_child =
            thread_record("side-child", Some("root"), "2026-08-21T12:00:00.000000007Z");
        side_child.side = StackReviewCommentSide::Left;
        let mut side_descendant = thread_record(
            "side-descendant",
            Some("side-child"),
            "2026-08-21T12:00:00.000000008Z",
        );
        side_descendant.side = StackReviewCommentSide::Left;

        let mut top_level_child = thread_record(
            "top-level-child",
            Some("root"),
            "2026-08-21T12:00:00.000000009Z",
        );
        top_level_child.path = None;
        top_level_child.start_row = None;
        top_level_child.start_column = None;
        top_level_child.end_row = None;
        top_level_child.end_column = None;
        top_level_child.side = StackReviewCommentSide::TopLevel;
        let mut top_level_descendant = thread_record(
            "top-level-descendant",
            Some("top-level-child"),
            "2026-08-21T12:00:00.000000010Z",
        );
        top_level_descendant.path = None;
        top_level_descendant.start_row = None;
        top_level_descendant.start_column = None;
        top_level_descendant.end_row = None;
        top_level_descendant.end_column = None;
        top_level_descendant.side = StackReviewCommentSide::TopLevel;

        let records = [
            root,
            normal_child,
            path_child,
            path_descendant,
            snapshot_child,
            snapshot_descendant,
            side_child,
            side_descendant,
            top_level_child,
            top_level_descendant,
        ];
        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build boundary-qualified comment thread index");

        let normal_key = index.thread_key_for("root").expect("normal key");
        assert_eq!(
            index
                .thread(normal_key)
                .expect("normal thread")
                .member_record_ids,
            ["root", "normal-child"]
        );
        for (child_id, descendant_id) in [
            ("path-child", "path-descendant"),
            ("snapshot-child", "snapshot-descendant"),
            ("side-child", "side-descendant"),
            ("top-level-child", "top-level-descendant"),
        ] {
            let key = index.thread_key_for(child_id).expect("boundary key");
            assert!(matches!(key, CommentThreadKey::BoundaryViolation { .. }));
            assert_ne!(key, normal_key);
            assert_eq!(
                index
                    .thread(key)
                    .expect("boundary thread")
                    .member_record_ids,
                [child_id, descendant_id]
            );
            assert_eq!(index.thread_key_for(descendant_id), Some(key));
        }
        let boundary_keys = [
            "path-child",
            "snapshot-child",
            "side-child",
            "top-level-child",
        ]
        .map(|record_id| index.thread_key_for(record_id).expect("partition key"));
        for (index, left) in boundary_keys.iter().enumerate() {
            for right in &boundary_keys[index.saturating_add(1)..] {
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn comment_thread_index_is_input_order_independent_for_acyclic_graphs() {
        let records = vec![
            thread_record("root-b", None, "2026-08-21T12:00:00.000000004Z"),
            thread_record("child-b", Some("root-b"), "2026-08-21T12:00:00.000000006Z"),
            thread_record("root-a", None, "2026-08-21T12:00:00.000000001Z"),
            thread_record(
                "child-a-2",
                Some("root-a"),
                "2026-08-21T12:00:00.000000003Z",
            ),
            thread_record(
                "child-a-1",
                Some("root-a"),
                "2026-08-21T12:00:00.000000002Z",
            ),
            thread_record("orphan", Some("missing"), "2026-08-21T12:00:00.000000005Z"),
        ];
        let reversed = records.iter().rev().cloned().collect::<Vec<_>>();

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build acyclic comment thread index");
        let reversed_index = CommentThreadIndex::new("base-head", reversed.iter())
            .expect("build reversed acyclic comment thread index");

        assert_eq!(index.threads(), reversed_index.threads());
        for record in &records {
            assert_eq!(
                index.thread_key_for(&record.id),
                reversed_index.thread_key_for(&record.id)
            );
        }
    }

    #[test]
    fn comment_thread_index_canonicalizes_cycles_independent_of_input_order() {
        let records = [
            thread_record("b", Some("c"), "2026-08-21T12:00:00.000000002Z"),
            thread_record("descendant", Some("b"), "2026-08-21T12:00:00.000000004Z"),
            thread_record("a", Some("b"), "2026-08-21T12:00:00.000000001Z"),
            thread_record("c", Some("a"), "2026-08-21T12:00:00.000000003Z"),
        ];
        let reversed = records.iter().rev().cloned().collect::<Vec<_>>();

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build cycle-degraded comment thread index");
        let reversed_index = CommentThreadIndex::new("base-head", reversed.iter())
            .expect("build reversed cycle-degraded comment thread index");
        let key = index.thread_key_for("descendant").expect("cycle key");
        assert!(matches!(
            key,
            CommentThreadKey::Cycle {
                canonical_member_id,
                ..
            } if canonical_member_id == "a"
        ));
        assert_eq!(reversed_index.thread_key_for("b"), Some(key));
        assert_eq!(
            index.thread(key).expect("cycle thread").member_record_ids,
            reversed_index
                .thread(key)
                .expect("reversed cycle thread")
                .member_record_ids
        );
        assert_eq!(
            index.thread(key).expect("cycle thread").member_record_ids,
            ["a", "c", "b", "descendant"]
        );
    }

    #[test]
    fn comment_thread_index_orders_normal_threads_by_their_roots() {
        let records = [
            thread_record("root-a", None, "2026-08-21T12:00:00.000000002Z"),
            thread_record("root-z", None, "2026-08-21T12:00:00.000000003Z"),
            thread_record(
                "early-child",
                Some("root-z"),
                "2026-08-21T12:00:00.000000001Z",
            ),
        ];

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build ordered comment thread index");
        let ordered_roots = index
            .threads()
            .iter()
            .map(|thread| thread.placement_record_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ordered_roots, ["root-a", "root-z"]);
    }

    #[test]
    fn comment_thread_index_orders_siblings_by_nanoseconds_then_stable_id() {
        let records = [
            thread_record("root", None, "2026-08-21T12:00:00Z"),
            thread_record("z", Some("root"), "2026-08-21T12:00:00.000000003Z"),
            thread_record("b", Some("root"), "2026-08-21T12:00:00.000000002Z"),
            thread_record("a", Some("root"), "2026-08-21T12:00:00.000000002Z"),
        ];

        let index = CommentThreadIndex::new("base-head", records.iter())
            .expect("build ordered comment thread index");
        let key = index.thread_key_for("root").expect("root key");

        assert_eq!(
            index.thread(key).expect("ordered thread").member_record_ids,
            ["root", "a", "b", "z"]
        );
    }

    #[test]
    fn comment_thread_key_has_a_stable_round_trip_representation() {
        let key = CommentThreadKey::MissingParent {
            partition: CommentThreadPartition {
                storage_key: "base-head".into(),
                base_oid: "base".into(),
                head_oid: "head".into(),
                path: Some("src/a b.rs".into()),
                side: StackReviewCommentSide::Left,
                class: StackReviewCommentClass::Inline,
            },
            missing_parent_id: "github:42".into(),
        };

        let serialized = key
            .to_stable_string()
            .expect("serialize stable comment thread key");

        assert_eq!(
            serialized,
            r#"{"kind":"missingParent","partition":{"storageKey":"base-head","baseOid":"base","headOid":"head","path":"src/a b.rs","side":"left","class":"inline"},"missing_parent_id":"github:42"}"#
        );
        assert_eq!(
            CommentThreadKey::from_stable_string(&serialized)
                .expect("deserialize stable comment thread key"),
            key
        );
    }

    use std::path::Path;

    async fn run_git(
        executor: gpui::BackgroundExecutor,
        repository: &Path,
        arguments: &[&str],
        timestamp: Option<i64>,
    ) {
        let git = GitBinary::new(
            "git".into(),
            repository.to_path_buf(),
            repository.join(".git"),
            executor,
            true,
        );
        let mut command = git.build_command(arguments);
        command
            .env("GIT_AUTHOR_NAME", "Stack Review Test")
            .env("GIT_AUTHOR_EMAIL", "stack-review@example.com")
            .env("GIT_COMMITTER_NAME", "Stack Review Test")
            .env("GIT_COMMITTER_EMAIL", "stack-review@example.com");
        if let Some(timestamp) = timestamp {
            let git_date = format!("@{timestamp} +0000");
            command
                .env("GIT_AUTHOR_DATE", &git_date)
                .env("GIT_COMMITTER_DATE", git_date);
        }
        let output = command.output().await.expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    const STACK_FILE: &str = r#"
        {
          "schemaVersion": 1,
          "repository": "github.com:example/repository",
          "stacks": [
            {
              "number": 42,
              "trunk": { "branch": "staging", "head": "1111111" },
              "branches": [
                {
                  "branch": "feature/foundation",
                  "base": "1111111",
                  "pullRequest": { "number": 101 }
                },
                {
                  "branch": "feature/ui",
                  "base": "2222222",
                  "pullRequest": { "number": 102 }
                }
              ]
            }
          ]
        }
    "#;

    #[test]
    fn parses_local_gh_stack_metadata() {
        let file = parse_stack_file(STACK_FILE).expect("valid stack metadata");

        assert_eq!(file.schema_version, 1);
        assert_eq!(file.stacks.len(), 1);
        assert_eq!(file.stacks[0].trunk.branch, "staging");
        assert_eq!(
            file.stacks[0]
                .branches
                .iter()
                .map(|branch| branch.branch.as_str())
                .collect::<Vec<_>>(),
            ["feature/foundation", "feature/ui"]
        );
        let serialized = file.to_json().expect("serialize local stack metadata");
        assert_eq!(
            parse_stack_file(&serialized).expect("reopen serialized stack metadata"),
            file
        );
    }

    #[test]
    fn selects_the_single_stack_containing_a_branch() {
        let file = parse_stack_file(STACK_FILE).expect("valid stack metadata");

        let stack = file
            .stack_for_branch("feature/ui")
            .expect("branch belongs to one stack");

        assert_eq!(stack.number, Some(42));
        assert_eq!(stack.base_branch_for("feature/foundation"), Some("staging"));
        assert_eq!(
            stack.base_branch_for("feature/ui"),
            Some("feature/foundation")
        );
    }

    #[test]
    fn rejects_unsupported_schema_versions() {
        let error = parse_stack_file(r#"{ "schemaVersion": 2, "stacks": [] }"#)
            .expect_err("unknown schema must fail");

        assert!(
            error
                .to_string()
                .contains("unsupported gh-stack schema version 2")
        );
    }

    #[gpui::test]
    async fn loads_a_direct_parent_diff_without_checkout(cx: &mut gpui::TestAppContext) {
        use crate::repository::RealGitRepository;
        use std::fs;

        cx.executor().allow_parking();
        let repository_directory = tempfile::tempdir().expect("temporary repository");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["init", "-b", "staging"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("modified.txt"), "before\n")
            .expect("write base file");
        fs::write(repository_directory.path().join("deleted.txt"), "deleted\n")
            .expect("write deleted file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "base"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "-c", "feature"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("modified.txt"), "after\n")
            .expect("modify file");
        fs::write(repository_directory.path().join("added.txt"), "added\n")
            .expect("write added file");
        fs::write(repository_directory.path().join("image.webp"), [0, 1, 2, 3])
            .expect("write binary file");
        fs::remove_file(repository_directory.path().join("deleted.txt")).expect("delete file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "feature"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "-c", "rewritten", "staging"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["cherry-pick", "feature"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "--amend", "-m", "rewritten feature"],
            None,
        )
        .await;

        let repository = RealGitRepository::new(
            &repository_directory.path().join(".git"),
            None,
            Some("git".into()),
            cx.executor(),
        )
        .expect("open repository");

        let feature_oid = repository
            .stack_review_resolve_revision("feature".into())
            .await
            .expect("resolve feature");
        assert_eq!(
            resolve_stack_review_commit_boundary(&repository, "staging", "feature", &feature_oid)
                .await
                .expect("validate feature boundary"),
            feature_oid
        );
        let rewritten_oid = repository
            .stack_review_resolve_revision("rewritten".into())
            .await
            .expect("resolve rewritten feature");
        assert_eq!(
            resolve_stack_review_commit_boundary(
                &repository,
                "staging",
                "rewritten",
                &feature_oid,
            )
            .await
            .expect("map stale source commit to rewritten equivalent"),
            rewritten_oid
        );
        assert!(
            resolve_stack_review_commit_boundary(&repository, "feature", "staging", "feature")
                .await
                .is_err(),
            "a boundary outside the selected ancestry must be rejected"
        );

        let diff = load_stack_diff(&repository, "staging", "feature")
            .await
            .expect("load stack diff");

        assert_eq!(diff.base_ref, "staging");
        assert_eq!(diff.head_ref, "feature");
        assert_eq!(diff.files.len(), 4);
        assert_eq!(
            diff.file("added.txt").expect("added file").status,
            StackReviewFileStatus::Added
        );
        assert_eq!(
            diff.file("added.txt")
                .expect("added file")
                .new_content
                .as_ref(),
            Some(&RevisionContent::Text("added\n".into()))
        );
        assert_eq!(
            diff.file("modified.txt")
                .expect("modified file")
                .old_content
                .as_ref(),
            Some(&RevisionContent::Text("before\n".into()))
        );
        assert_eq!(
            diff.file("modified.txt")
                .expect("modified file")
                .new_content
                .as_ref(),
            Some(&RevisionContent::Text("after\n".into()))
        );
        assert_eq!(
            diff.file("deleted.txt").expect("deleted file").new_content,
            None
        );
        assert_eq!(
            diff.file("image.webp").expect("binary file").content_kind,
            StackReviewContentKind::Binary
        );
    }

    #[gpui::test]
    async fn excludes_merge_only_files_from_the_review_diff(cx: &mut gpui::TestAppContext) {
        use crate::repository::RealGitRepository;
        use std::fs;

        cx.executor().allow_parking();
        let repository_directory = tempfile::tempdir().expect("temporary repository");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["init", "-b", "staging"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("both.txt"), "base\n").expect("write base file");
        fs::write(
            repository_directory.path().join("rename-old.txt"),
            "renamed\n",
        )
        .expect("write file to rename");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "base"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "-c", "feature"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("direct.txt"), "direct\n")
            .expect("write direct file");
        fs::write(repository_directory.path().join("both.txt"), "direct\n")
            .expect("write direct update");
        fs::rename(
            repository_directory.path().join("rename-old.txt"),
            repository_directory.path().join("rename-new.txt"),
        )
        .expect("rename direct file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "direct changes"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "-c", "integration"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("merged.txt"), "merged\n")
            .expect("write merged file");
        fs::write(repository_directory.path().join("both.txt"), "merged\n")
            .expect("write merged update");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "integration changes"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "feature"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["merge", "--no-ff", "integration", "-m", "merge integration"],
            None,
        )
        .await;

        let repository = RealGitRepository::new(
            &repository_directory.path().join(".git"),
            None,
            Some("git".into()),
            cx.executor(),
        )
        .expect("open repository");

        let diff = load_stack_diff(&repository, "staging", "feature")
            .await
            .expect("load stack diff");

        assert_eq!(
            diff.file("direct.txt").expect("direct file").provenance,
            StackReviewFileProvenance::Direct
        );
        assert_eq!(
            diff.file("rename-old.txt")
                .expect("deleted rename source")
                .provenance,
            StackReviewFileProvenance::Direct
        );
        assert_eq!(
            diff.file("rename-new.txt")
                .expect("added rename target")
                .provenance,
            StackReviewFileProvenance::Direct
        );
        assert!(diff.file("merged.txt").is_none());
        assert_eq!(
            diff.file("both.txt").expect("mixed file").provenance,
            StackReviewFileProvenance::Mixed
        );
        assert_eq!(
            diff.provenance_counts(),
            StackReviewProvenanceCounts {
                direct: 3,
                merge: 0,
                mixed: 1,
                unknown: 0,
            }
        );
    }

    #[gpui::test]
    async fn filters_a_stack_diff_by_author_time(cx: &mut gpui::TestAppContext) {
        use crate::repository::RealGitRepository;
        use std::fs;

        cx.executor().allow_parking();
        let repository_directory = tempfile::tempdir().expect("temporary repository");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["init", "-b", "staging"],
            Some(1_000_000_000),
        )
        .await;
        fs::write(repository_directory.path().join("base.txt"), "base\n").expect("write base file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            Some(1_000_000_000),
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "base"],
            Some(1_000_000_000),
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "-c", "feature"],
            Some(1_000_000_000),
        )
        .await;
        fs::write(repository_directory.path().join("old.txt"), "old\n").expect("write old file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            Some(1_000_000_100),
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "old"],
            Some(1_000_000_100),
        )
        .await;
        fs::write(repository_directory.path().join("recent.txt"), "recent\n")
            .expect("write recent file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            Some(1_000_000_200),
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "recent"],
            Some(1_000_000_200),
        )
        .await;
        fs::write(
            repository_directory.path().join("old_after_restack.txt"),
            "old authored work\n",
        )
        .expect("write old authored file after recent commit");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            Some(1_000_000_120),
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "old authored after restack"],
            Some(1_000_000_120),
        )
        .await;

        let repository = RealGitRepository::new(
            &repository_directory.path().join(".git"),
            None,
            Some("git".into()),
            cx.executor(),
        )
        .expect("open repository");

        let diff = load_stack_diff_since(&repository, "staging", "feature", 1_000_000_150)
            .await
            .expect("load filtered stack diff");

        assert!(diff.file("old.txt").is_none());
        assert!(diff.file("old_after_restack.txt").is_none());
        assert_eq!(diff.base_ref, "staging");
        assert_eq!(diff.head_ref, "feature");
        assert_eq!(
            diff.file("recent.txt")
                .expect("recent file")
                .new_content
                .as_ref(),
            Some(&RevisionContent::Text("recent\n".into()))
        );
    }

    #[gpui::test]
    async fn reports_a_declared_layer_without_parent_ancestry(cx: &mut gpui::TestAppContext) {
        use crate::repository::RealGitRepository;
        use std::fs;

        cx.executor().allow_parking();
        let repository_directory = tempfile::tempdir().expect("temporary repository");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["init", "-b", "staging"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("base.txt"), "base\n").expect("write base file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "base"],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["switch", "--orphan", "unrelated"],
            None,
        )
        .await;
        fs::write(repository_directory.path().join("other.txt"), "other\n")
            .expect("write unrelated file");
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["add", "."],
            None,
        )
        .await;
        run_git(
            cx.executor(),
            repository_directory.path(),
            &["commit", "-m", "unrelated"],
            None,
        )
        .await;

        let repository = RealGitRepository::new(
            &repository_directory.path().join(".git"),
            None,
            Some("git".into()),
            cx.executor(),
        )
        .expect("open repository");
        let branch_heads = repository
            .revparse_batch(vec!["staging".into(), "unrelated".into()])
            .await
            .expect("resolve refs");
        let snapshot = StackSnapshot {
            number: None,
            trunk: ResolvedStackBranch {
                branch: "staging".into(),
                oid: branch_heads[0].clone().expect("staging oid"),
                pull_request_number: None,
            },
            layers: vec![ResolvedStackLayer {
                base: ResolvedStackBranch {
                    branch: "staging".into(),
                    oid: branch_heads[0].clone().expect("staging oid"),
                    pull_request_number: None,
                },
                head: ResolvedStackBranch {
                    branch: "unrelated".into(),
                    oid: branch_heads[1].clone().expect("unrelated oid"),
                    pull_request_number: None,
                },
            }],
        };

        let ancestry = inspect_stack_ancestry(&repository, &snapshot)
            .await
            .expect("inspect stack ancestry");

        assert_eq!(ancestry.len(), 1);
        assert_eq!(ancestry[0].base_branch, "staging");
        assert_eq!(ancestry[0].head_branch, "unrelated");
        assert!(!ancestry[0].is_ancestor);
    }

    #[test]
    fn round_trips_reviewed_files_without_crossing_snapshot_boundaries() {
        let mut state = StackReviewState::new("base-a", "head-a");
        state.set_file_reviewed("src/a.rs", "fingerprint-a", true);
        state.set_file_reviewed("src/b.rs", "fingerprint-b", false);
        let json = state.to_json().expect("serialize review state");
        let restored = StackReviewState::from_json(&json).expect("deserialize review state");

        assert!(restored.is_file_reviewed("src/a.rs", "fingerprint-a"));
        assert!(!restored.is_file_reviewed("src/a.rs", "changed-fingerprint"));
        assert!(!restored.is_file_reviewed("src/b.rs", "fingerprint-b"));
        assert!(restored.matches_snapshot("base-a", "head-a"));
        assert!(!restored.matches_snapshot("base-a", "head-b"));
    }

    #[test]
    fn time_checkpoint_uses_parent_of_first_qualifying_graph_commit() {
        let commits = vec![
            FirstParentCommit {
                oid: "one".into(),
                author_timestamp: 100,
                is_merge: false,
                paths: vec![],
            },
            FirstParentCommit {
                oid: "two".into(),
                author_timestamp: 300,
                is_merge: false,
                paths: vec![],
            },
            FirstParentCommit {
                oid: "three".into(),
                author_timestamp: 200,
                is_merge: false,
                paths: vec![],
            },
        ];
        assert_eq!(time_checkpoint_base("base", "head", &commits, 250), "one");
        assert_eq!(time_checkpoint_base("base", "head", &commits, 50), "base");
        assert_eq!(time_checkpoint_base("base", "head", &commits, 400), "head");
    }

    #[test]
    fn stack_review_line_counts_match_native_line_diff_semantics() {
        assert_eq!(
            stack_review_line_counts("one\ntwo\n", "one\nchanged\nthree\n"),
            (2, 1)
        );
    }

    #[test]
    fn round_trips_and_validates_per_comment_records_and_current_manifest() {
        let record = StackReviewCommentRecord {
            schema_version: COMMENT_SCHEMA_VERSION,
            id: "018f0f52-7f87-7b4f-a940-98f22a49f777".into(),
            base_oid: "base-a".into(),
            head_oid: "head-a".into(),
            path: Some("src/lib.rs".into()),
            side: StackReviewCommentSide::Right,
            start_row: Some(4),
            start_column: Some(2),
            end_row: Some(6),
            end_column: Some(8),
            body: "Check this edge case".into(),
            author: StackReviewCommentAuthor {
                name: "Claude Code".into(),
                login: None,
            },
            source: StackReviewCommentSource::LocalAgent,
            reply_to: None,
            created_at: "2026-08-21T12:00:00Z".into(),
            updated_at: "2026-08-21T12:00:00Z".into(),
            outdated: false,
            resolved: false,
            local_resolution: None,
            github: None,
        };
        let restored = StackReviewCommentRecord::from_json(
            &record.to_json().expect("serialize comment"),
            "base-a",
            "head-a",
        )
        .expect("deserialize comment");
        assert_eq!(restored, record);
        assert!(restored.is_writable());
        assert!(
            StackReviewCommentRecord::from_json(
                &record.to_json().expect("serialize comment"),
                "other-base",
                "head-a",
            )
            .is_err()
        );

        let manifest = StackReviewCurrentManifest::new(
            "/repo".into(),
            "base-a".into(),
            "head-a".into(),
            "reviews/base-a-head-a.json".into(),
            "comments/base-a-head-a".into(),
            "github/base-a-head-a".into(),
            vec![12, 13],
        );
        assert_eq!(
            StackReviewCurrentManifest::from_json(&manifest.to_json().expect("serialize manifest"))
                .expect("deserialize manifest"),
            manifest
        );
    }

    #[test]
    fn round_trips_snapshot_bound_review_comments() {
        let mut state = StackReviewState::new("base-a", "head-a");
        state.set_comments(vec![StackReviewComment {
            id: 7,
            record_id: Some("record-7".into()),
            path: "src/lib.rs".into(),
            start_row: 4,
            start_column: 2,
            end_row: 6,
            end_column: 8,
            body: "Check this edge case".into(),
            created_at: "2026-08-21T12:00:00Z".into(),
            resolved: false,
            author: StackReviewCommentAuthor {
                name: "Claude Code".into(),
                login: None,
            },
            source: StackReviewCommentSource::LocalAgent,
            reply_to: Some(3),
            reply_to_record_id: Some("record-3".into()),
        }]);

        let restored = StackReviewState::from_json(&state.to_json().expect("serialize state"))
            .expect("deserialize state");

        assert_eq!(restored.comments().len(), 1);
        assert_eq!(restored.comments()[0].id, 7);
        assert_eq!(
            restored.comments()[0].record_id.as_deref(),
            Some("record-7")
        );
        assert_eq!(restored.comments()[0].path, "src/lib.rs");
        assert_eq!(restored.comments()[0].start_row, 4);
        assert_eq!(restored.comments()[0].end_column, 8);
        assert_eq!(restored.comments()[0].body, "Check this edge case");
        assert_eq!(restored.comments()[0].author.name, "Claude Code");
        assert_eq!(
            restored.comments()[0].source,
            StackReviewCommentSource::LocalAgent
        );
        assert_eq!(restored.comments()[0].reply_to, Some(3));
        assert_eq!(
            restored.comments()[0].reply_to_record_id.as_deref(),
            Some("record-3")
        );
    }

    #[test]
    fn migrates_legacy_comment_defaults_and_duplicate_ids() {
        let restored = StackReviewState::from_json(
            r#"{
                "schemaVersion": 1,
                "baseOid": "base",
                "headOid": "head",
                "reviewedFiles": [],
                "comments": [
                    {"id":0,"path":"a.rs","startRow":1,"startColumn":0,"endRow":1,"endColumn":1,"body":"one"},
                    {"id":0,"path":"b.rs","startRow":2,"startColumn":0,"endRow":2,"endColumn":1,"body":"two"}
                ]
            }"#,
        )
        .expect("migrate legacy comments");

        assert_eq!(restored.comments()[0].id, 0);
        assert_eq!(restored.comments()[1].id, 1);
        assert_eq!(restored.comments()[0].author.name, "You");
        assert_eq!(
            restored.comments()[0].source,
            StackReviewCommentSource::LocalHuman
        );
        assert_eq!(restored.comments()[0].reply_to, None);
        assert_eq!(restored.comments()[0].record_id, None);
        assert_eq!(restored.comments()[0].reply_to_record_id, None);
    }

    #[test]
    fn reconciling_visible_comments_preserves_unmapped_comments() {
        let mut state = StackReviewState::new("base", "head");
        state.set_comments(vec![
            StackReviewComment {
                id: 1,
                record_id: None,
                path: "src/lib.rs".into(),
                start_row: 1,
                start_column: 0,
                end_row: 1,
                end_column: 1,
                body: "Rendered".into(),
                created_at: String::new(),
                resolved: false,
                author: StackReviewCommentAuthor::default(),
                source: StackReviewCommentSource::LocalHuman,
                reply_to: None,
                reply_to_record_id: None,
            },
            StackReviewComment {
                id: 2,
                record_id: None,
                path: "src/lib.rs".into(),
                start_row: 200,
                start_column: 0,
                end_row: 200,
                end_column: 1,
                body: "Outdated but preserved".into(),
                created_at: String::new(),
                resolved: false,
                author: StackReviewCommentAuthor::default(),
                source: StackReviewCommentSource::LocalHuman,
                reply_to: None,
                reply_to_record_id: None,
            },
        ]);

        state.reconcile_rendered_comments_for_path("src/lib.rs", &HashSet::from([1]), Vec::new());

        assert_eq!(state.comments().len(), 1);
        assert_eq!(state.comments()[0].id, 2);
        assert_eq!(state.comments()[0].body, "Outdated but preserved");
    }

    #[test]
    fn rejects_review_state_from_an_unknown_schema() {
        let error = StackReviewState::from_json(
            r#"{"schemaVersion":99,"baseOid":"a","headOid":"b","reviewedFiles":[]}"#,
        )
        .expect_err("unknown schema must fail");

        assert!(
            error
                .to_string()
                .contains("unsupported review-state schema")
        );
    }

    #[test]
    fn resolves_a_stack_to_immutable_branch_oids() {
        use std::collections::HashMap;

        let file = parse_stack_file(STACK_FILE).expect("valid stack metadata");
        let stack = file
            .stack_for_branch("feature/ui")
            .expect("branch belongs to one stack");
        let branch_heads = HashMap::from([
            ("staging".to_string(), "aaaaaaaa".to_string()),
            ("feature/foundation".to_string(), "bbbbbbbb".to_string()),
            ("feature/ui".to_string(), "cccccccc".to_string()),
        ]);

        let snapshot = stack
            .resolve(&branch_heads)
            .expect("all stack refs resolve");

        assert_eq!(snapshot.trunk.branch, "staging");
        assert_eq!(snapshot.trunk.oid, "aaaaaaaa");
        assert_eq!(snapshot.layers.len(), 2);
        assert_eq!(snapshot.layers[0].base.oid, "aaaaaaaa");
        assert_eq!(snapshot.layers[0].head.oid, "bbbbbbbb");
        assert_eq!(snapshot.layers[1].base.oid, "bbbbbbbb");
        assert_eq!(snapshot.layers[1].head.oid, "cccccccc");
        assert_eq!(
            snapshot
                .boundary(0)
                .map(|boundary| boundary.branch.as_str()),
            Some("staging")
        );
        assert_eq!(
            snapshot
                .boundary(2)
                .map(|boundary| boundary.branch.as_str()),
            Some("feature/ui")
        );
        assert_eq!(snapshot.refs_between(0, 2), Some(("aaaaaaaa", "cccccccc")));
        assert_eq!(snapshot.refs_between(2, 1), None);
    }

    #[test]
    fn rejects_a_stack_snapshot_with_a_missing_branch() {
        use std::collections::HashMap;

        let file = parse_stack_file(STACK_FILE).expect("valid stack metadata");
        let stack = file
            .stack_for_branch("feature/ui")
            .expect("branch belongs to one stack");
        let branch_heads = HashMap::from([
            ("staging".to_string(), "aaaaaaaa".to_string()),
            ("feature/ui".to_string(), "cccccccc".to_string()),
        ]);

        let error = stack
            .resolve(&branch_heads)
            .expect_err("missing branch must fail");

        assert!(
            error
                .to_string()
                .contains("missing local branch \"feature/foundation\"")
        );
    }

    #[test]
    fn reports_ambiguous_stack_membership() {
        let file = parse_stack_file(
            r#"
            {
              "schemaVersion": 1,
              "stacks": [
                {
                  "trunk": { "branch": "staging" },
                  "branches": [{ "branch": "feature/shared" }]
                },
                {
                  "trunk": { "branch": "main" },
                  "branches": [{ "branch": "feature/shared" }]
                }
              ]
            }
            "#,
        )
        .expect("valid stack metadata");

        let error = file
            .stack_for_branch("feature/shared")
            .expect_err("ambiguous branch must fail");

        assert!(error.to_string().contains("belongs to 2 local stacks"));
    }
}
