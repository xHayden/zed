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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    let tree_diff = repository
        .stack_review_diff_tree(base_ref.to_owned(), head_ref.to_owned())
        .await?;
    let mut entries = tree_diff.entries.into_iter().collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));

    let commits = repository
        .first_parent_commits(base_ref.to_owned(), head_ref.to_owned())
        .await?;
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
        .into_iter()
        .filter(|commit| commit.author_timestamp >= author_timestamp)
        .flat_map(|commit| commit.paths)
        .collect::<HashSet<_>>();
    let mut diff = load_stack_diff(repository, base_ref, head_ref).await?;
    diff.files
        .retain(|file| included_paths.contains(&file.path));
    Ok(diff)
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
        }]);

        let restored = StackReviewState::from_json(&state.to_json().expect("serialize state"))
            .expect("deserialize state");

        assert_eq!(restored.comments().len(), 1);
        assert_eq!(restored.comments()[0].id, 7);
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
    }

    #[test]
    fn reconciling_visible_comments_preserves_unmapped_comments() {
        let mut state = StackReviewState::new("base", "head");
        state.set_comments(vec![
            StackReviewComment {
                id: 1,
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
            },
            StackReviewComment {
                id: 2,
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
