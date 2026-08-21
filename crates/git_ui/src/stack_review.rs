use crate::multi_diff_view::{ContentDiffEntry, MultiDiffView};
use anyhow::{Context as _, Result, anyhow};
use editor::{Editor, EditorEvent};
use feature_flags::{FeatureFlagAppExt as _, StackReviewFeatureFlag};
use fs::{Fs, RemoveOptions};
use futures::StreamExt as _;
use git::{
    repository::RevisionContent,
    stack_review::{
        BranchRef, PullRequestRef, Stack, StackFile, StackReviewComment, StackReviewCommentAuthor,
        StackReviewCommentRecord, StackReviewCommentSide, StackReviewCommentSource,
        StackReviewContentKind, StackReviewCurrentManifest, StackReviewFileProvenance,
        StackReviewFileStatus, StackReviewGitHubCommentIdentity, StackReviewGitHubCommentKind,
        StackReviewState, StackSnapshot, parse_stack_file,
    },
};
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, ClipboardItem, Context, DragMoveEvent,
    Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Pixels, PromptLevel,
    Render, SharedString, Subscription, Task, Window, actions, deferred, prelude::*, px,
    uniform_list,
};
use project::{Project, git_store::Repository};
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use time::OffsetDateTime;
use ui::{
    Button, Checkbox, Color, ContextMenu, DropdownMenu, Icon, IconName, Label, LabelCommon as _,
    ListItem, ListItemSpacing, Toggleable as _, prelude::*,
};
use uuid::Uuid;
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
    notifications::NotifyTaskExt,
    searchable::SearchableItemHandle,
};

actions!(
    git,
    [
        /// Reviews the current branch as part of its local GitHub stack.
        ReviewStack,
        /// Selects the next visible Stack Review file.
        StackReviewNextFile,
        /// Selects the previous visible Stack Review file.
        StackReviewPreviousFile,
        /// Shows or hides test files in Stack Review.
        StackReviewToggleTests,
        /// Shows or hides migration files in Stack Review.
        StackReviewToggleMigrations,
    ]
);

#[derive(Clone)]
struct DraggedStackReviewSidebar;

const STACK_REVIEW_SIDEBAR_DEFAULT_WIDTH: Pixels = px(280.);
const STACK_REVIEW_SIDEBAR_MIN_WIDTH: Pixels = px(200.);
const STACK_REVIEW_SIDEBAR_MAX_WIDTH: Pixels = px(720.);

impl Render for DraggedStackReviewSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubPullRequest {
    number: u32,
    head_ref_name: String,
    base_ref_name: String,
}

#[derive(Debug)]
enum GitHubStackDiscoveryError {
    CliNotInstalled,
    NotAuthenticated(String),
    CommandFailed(String),
    InvalidResponse(anyhow::Error),
    NoOpenPullRequest(String),
    Cycle(String),
}

impl fmt::Display for GitHubStackDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CliNotInstalled => {
                write!(
                    formatter,
                    "GitHub CLI is not installed; run `brew install gh`"
                )
            }
            Self::NotAuthenticated(detail) => write!(
                formatter,
                "GitHub CLI authentication is required; run `gh auth login` ({detail})"
            ),
            Self::CommandFailed(detail) => write!(formatter, "GitHub CLI failed: {detail}"),
            Self::InvalidResponse(error) => {
                write!(formatter, "invalid GitHub CLI response: {error}")
            }
            Self::NoOpenPullRequest(branch) => {
                write!(
                    formatter,
                    "branch {branch:?} has no open GitHub pull request"
                )
            }
            Self::Cycle(branch) => {
                write!(formatter, "GitHub PR stack contains a cycle at {branch:?}")
            }
        }
    }
}

impl std::error::Error for GitHubStackDiscoveryError {}

fn is_common_stack_trunk(branch: &str) -> bool {
    ["staging", "develop", "development", "main", "master"]
        .iter()
        .any(|candidate| branch.eq_ignore_ascii_case(candidate))
}

async fn prompt_to_configure_github_cli(
    error: &GitHubStackDiscoveryError,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let Some((message, detail, command)) = (match error {
        GitHubStackDiscoveryError::CliNotInstalled => Some((
            "Install GitHub CLI",
            "Stack Review uses `gh` read-only to discover GitHub PR relationships.",
            "brew install gh",
        )),
        GitHubStackDiscoveryError::NotAuthenticated(_) => Some((
            "Connect GitHub CLI",
            "Authenticate `gh`, then run Git: Review Stack again. Zed never reads your token.",
            "gh auth login",
        )),
        _ => None,
    }) else {
        return Ok(());
    };
    let response = cx.update(|window, cx| {
        window.prompt(
            PromptLevel::Info,
            message,
            Some(detail),
            &["Copy Command", "Cancel"],
            cx,
        )
    })?;
    if response.await? == 0 {
        cx.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(command.to_owned()));
        })?;
    }
    Ok(())
}

async fn discover_stack_with_github_cli(
    work_directory: &Path,
    current_branch: &str,
) -> std::result::Result<StackFile, GitHubStackDiscoveryError> {
    let auth = util::command::new_command("gh")
        .args(["auth", "status", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(work_directory)
        .output()
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                GitHubStackDiscoveryError::CliNotInstalled
            } else {
                GitHubStackDiscoveryError::CommandFailed(error.to_string())
            }
        })?;
    if !auth.status.success() {
        return Err(GitHubStackDiscoveryError::NotAuthenticated(
            String::from_utf8_lossy(&auth.stderr).trim().to_owned(),
        ));
    }

    let mut branch = current_branch.to_owned();
    let mut pull_requests = Vec::new();
    let mut seen = HashSet::new();
    loop {
        if !pull_requests.is_empty() && is_common_stack_trunk(&branch) {
            break;
        }
        if !seen.insert(branch.clone()) {
            return Err(GitHubStackDiscoveryError::Cycle(branch));
        }
        let output = util::command::new_command("gh")
            .args([
                "pr",
                "list",
                "--head",
                &branch,
                "--state",
                "open",
                "--limit",
                "1",
                "--json",
                "number,headRefName,baseRefName",
            ])
            .env("GH_PROMPT_DISABLED", "1")
            .current_dir(work_directory)
            .output()
            .await
            .map_err(|error| GitHubStackDiscoveryError::CommandFailed(error.to_string()))?;
        if !output.status.success() {
            return Err(GitHubStackDiscoveryError::CommandFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let mut matches: Vec<GitHubPullRequest> = serde_json::from_slice(&output.stdout)
            .map_err(|error| GitHubStackDiscoveryError::InvalidResponse(error.into()))?;
        let Some(pull_request) = matches.pop() else {
            if pull_requests.is_empty() {
                return Err(GitHubStackDiscoveryError::NoOpenPullRequest(branch));
            }
            break;
        };
        branch = pull_request.base_ref_name.clone();
        pull_requests.push(pull_request);
    }

    stack_file_from_github_prs(current_branch, pull_requests)
        .map_err(GitHubStackDiscoveryError::InvalidResponse)
}

async fn github_api_values(
    work_directory: &Path,
    endpoint: &str,
) -> Result<Vec<serde_json::Value>> {
    let output = util::command::new_command("gh")
        .args(["api", "--paginate", "--slurp", endpoint])
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(work_directory)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "GitHub API request failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let pages: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&output.stdout)?;
    Ok(pages.into_iter().flatten().collect())
}

async fn github_api_value(work_directory: &Path, endpoint: &str) -> Result<serde_json::Value> {
    let output = util::command::new_command("gh")
        .args(["api", endpoint])
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(work_directory)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "GitHub API request failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn github_review_thread_states(
    work_directory: &Path,
    repository: &str,
    pull_request_number: u32,
) -> Result<HashMap<u64, (bool, bool)>> {
    let (owner, name) = repository
        .split_once('/')
        .context("GitHub repository name is missing its owner")?;
    let query = r#"
        query($owner: String!, $name: String!, $number: Int!, $after: String) {
          repository(owner: $owner, name: $name) {
            pullRequest(number: $number) {
              reviewThreads(first: 100, after: $after) {
                pageInfo { hasNextPage endCursor }
                nodes {
                  isResolved
                  isOutdated
                  comments(first: 100) { nodes { databaseId } }
                }
              }
            }
          }
        }
    "#;
    let mut states = HashMap::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut arguments = vec![
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={query}"),
            "-F".to_owned(),
            format!("owner={owner}"),
            "-F".to_owned(),
            format!("name={name}"),
            "-F".to_owned(),
            format!("number={pull_request_number}"),
        ];
        if let Some(cursor) = &cursor {
            arguments.extend(["-F".to_owned(), format!("after={cursor}")]);
        }
        let output = util::command::new_command("gh")
            .args(arguments)
            .env("GH_PROMPT_DISABLED", "1")
            .current_dir(work_directory)
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "GitHub review-thread lookup failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let threads = response
            .pointer("/data/repository/pullRequest/reviewThreads")
            .context("GitHub review-thread lookup returned no threads")?;
        for thread in threads
            .get("nodes")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let resolved = thread
                .get("isResolved")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let outdated = thread
                .get("isOutdated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            for comment in thread
                .pointer("/comments/nodes")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(database_id) = comment
                    .get("databaseId")
                    .and_then(serde_json::Value::as_u64)
                {
                    states.insert(database_id, (resolved, outdated));
                }
            }
        }
        let page_info = threads
            .get("pageInfo")
            .context("GitHub review-thread lookup returned no page info")?;
        if !page_info
            .get("hasNextPage")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            break;
        }
        cursor = page_info
            .get("endCursor")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        anyhow::ensure!(
            cursor.is_some(),
            "GitHub review-thread pagination has no cursor"
        );
    }
    Ok(states)
}

fn github_author(value: &serde_json::Value) -> StackReviewCommentAuthor {
    let login = value
        .pointer("/user/login")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    StackReviewCommentAuthor {
        name: login.clone(),
        login: Some(login),
    }
}

fn github_comment_records(
    pull_request_number: u32,
    base_oid: &str,
    head_oid: &str,
    inline_comments: Vec<serde_json::Value>,
    reviews: Vec<serde_json::Value>,
    conversation_comments: Vec<serde_json::Value>,
    thread_states: &HashMap<u64, (bool, bool)>,
    force_outdated: bool,
) -> Result<Vec<StackReviewCommentRecord>> {
    let mut records = Vec::new();
    for value in inline_comments {
        let github_id = value
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .context("GitHub inline comment has no id")?;
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .context("GitHub inline comment has no path")?;
        let current_line = value.get("line").and_then(serde_json::Value::as_u64);
        let line = current_line
            .or_else(|| {
                value
                    .get("original_line")
                    .and_then(serde_json::Value::as_u64)
            })
            .context("GitHub inline comment has no line")?;
        let side = match value
            .get("side")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("RIGHT")
        {
            "LEFT" => StackReviewCommentSide::Left,
            _ => StackReviewCommentSide::Right,
        };
        let id = format!("github-pr-{pull_request_number}-inline-{github_id}");
        let (resolved, thread_outdated) =
            thread_states.get(&github_id).copied().unwrap_or_default();
        let mut record = StackReviewCommentRecord::new_github_inline(
            id,
            base_oid.to_owned(),
            head_oid.to_owned(),
            path.to_owned(),
            side,
            u32::try_from(line.saturating_sub(1)).context("GitHub line exceeds u32")?,
            value
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            github_author(&value),
            value
                .get("in_reply_to_id")
                .and_then(serde_json::Value::as_u64)
                .map(|id| format!("github-pr-{pull_request_number}-inline-{id}")),
            value
                .get("created_at")
                .or_else(|| value.get("updated_at"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            StackReviewGitHubCommentIdentity {
                pull_request_number,
                github_id: github_id.to_string(),
                url: value
                    .get("html_url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                kind: StackReviewGitHubCommentKind::Inline,
                commit_oid: value
                    .get("commit_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            },
            current_line.is_none() || thread_outdated || force_outdated,
        );
        record.updated_at = value
            .get("updated_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(record.created_at.as_str())
            .to_owned();
        record.resolved = resolved;
        records.push(record);
    }
    for (kind, values) in [
        (StackReviewGitHubCommentKind::Review, reviews),
        (
            StackReviewGitHubCommentKind::Conversation,
            conversation_comments,
        ),
    ] {
        for value in values {
            let body = value
                .get("body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim();
            if body.is_empty() {
                continue;
            }
            let github_id = value
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .context("GitHub top-level comment has no id")?;
            let kind_name = match kind {
                StackReviewGitHubCommentKind::Review => "review",
                StackReviewGitHubCommentKind::Conversation => "conversation",
                StackReviewGitHubCommentKind::Inline => continue,
            };
            records.push(StackReviewCommentRecord::new_github_top_level(
                format!("github-pr-{pull_request_number}-{kind_name}-{github_id}"),
                base_oid.to_owned(),
                head_oid.to_owned(),
                body.to_owned(),
                github_author(&value),
                value
                    .get("submitted_at")
                    .or_else(|| value.get("updated_at"))
                    .or_else(|| value.get("created_at"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                StackReviewGitHubCommentIdentity {
                    pull_request_number,
                    github_id: github_id.to_string(),
                    url: value
                        .get("html_url")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    kind,
                    commit_oid: value
                        .get("commit_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                },
            ));
        }
    }
    Ok(records)
}

async fn refresh_github_comment_projection(
    fs: &Arc<dyn Fs>,
    work_directory: &Path,
    github_directory: &Path,
    base_oid: &str,
    head_oid: &str,
    pull_requests: &[(u32, String)],
) -> Result<()> {
    if pull_requests.is_empty() {
        return Ok(());
    }
    let output = util::command::new_command("gh")
        .args(["repo", "view", "--json", "nameWithOwner"])
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(work_directory)
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "GitHub repository lookup failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let repository: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let repository = repository
        .get("nameWithOwner")
        .and_then(serde_json::Value::as_str)
        .context("GitHub repository lookup returned no nameWithOwner")?;
    let mut records = Vec::new();
    for (pull_request_number, expected_head_oid) in pull_requests {
        let pull_request = github_api_value(
            work_directory,
            &format!("repos/{repository}/pulls/{pull_request_number}"),
        )
        .await?;
        let remote_head_oid = pull_request
            .pointer("/head/sha")
            .and_then(serde_json::Value::as_str)
            .context("GitHub pull request returned no head SHA")?;
        let force_outdated = remote_head_oid != expected_head_oid;
        let thread_states =
            github_review_thread_states(work_directory, repository, *pull_request_number).await?;
        let inline = github_api_values(
            work_directory,
            &format!("repos/{repository}/pulls/{pull_request_number}/comments"),
        )
        .await?;
        let reviews = github_api_values(
            work_directory,
            &format!("repos/{repository}/pulls/{pull_request_number}/reviews"),
        )
        .await?;
        let conversation = github_api_values(
            work_directory,
            &format!("repos/{repository}/issues/{pull_request_number}/comments"),
        )
        .await?;
        records.extend(github_comment_records(
            *pull_request_number,
            base_oid,
            head_oid,
            inline,
            reviews,
            conversation,
            &thread_states,
            force_outdated,
        )?);
    }
    let mut expected_paths = HashSet::new();
    for mut record in records {
        let path = github_directory.join(comment_file_name(&record));
        if fs.is_file(&path).await {
            match fs.load(&path).await.and_then(|serialized| {
                StackReviewCommentRecord::from_json(&serialized, base_oid, head_oid)
            }) {
                Ok(existing) => record.local_resolution = existing.local_resolution,
                Err(error) => {
                    log::warn!(
                        "unable to preserve local comment resolution at {path:?}: {error:#}"
                    );
                }
            }
        }
        expected_paths.insert(path.clone());
        fs.atomic_write(path, record.to_json()?).await?;
    }
    let mut existing = fs.read_dir(github_directory).await?;
    while let Some(path) = existing.next().await {
        let path = path?;
        if path.extension().and_then(|extension| extension.to_str()) == Some("json")
            && !expected_paths.contains(&path)
        {
            fs.remove_file(
                &path,
                RemoveOptions {
                    ignore_if_not_exists: true,
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    Ok(())
}

fn stack_file_from_github_prs(
    current_branch: &str,
    pull_requests: Vec<GitHubPullRequest>,
) -> Result<StackFile> {
    anyhow::ensure!(
        !pull_requests.is_empty(),
        "current branch has no open GitHub PR"
    );
    let mut expected_head = current_branch;
    let mut branches = Vec::with_capacity(pull_requests.len());
    for pull_request in &pull_requests {
        if !branches.is_empty() && is_common_stack_trunk(&pull_request.head_ref_name) {
            break;
        }
        anyhow::ensure!(
            pull_request.head_ref_name == expected_head,
            "GitHub PR chain is discontinuous at {:?}",
            pull_request.head_ref_name
        );
        branches.push(BranchRef {
            branch: pull_request.head_ref_name.clone(),
            head: None,
            base: None,
            pull_request: Some(PullRequestRef {
                number: pull_request.number,
                id: None,
                url: None,
                merged: false,
            }),
        });
        expected_head = &pull_request.base_ref_name;
    }
    branches.reverse();
    Ok(StackFile {
        schema_version: 1,
        repository: None,
        stacks: vec![Stack {
            id: Some(format!("github:{current_branch}")),
            number: None,
            trunk: BranchRef {
                branch: expected_head.to_owned(),
                head: None,
                base: None,
                pull_request: None,
            },
            branches,
        }],
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StackReviewScope {
    AggregateThrough(usize),
    Layer(usize),
    Range { from: usize, to: usize },
}

impl StackReviewScope {
    fn refs<'a>(&self, snapshot: &'a StackSnapshot) -> Option<(&'a str, &'a str)> {
        match self {
            Self::AggregateThrough(index) => snapshot.refs_between(0, index.saturating_add(1)),
            Self::Layer(index) => snapshot.refs_between(*index, index.saturating_add(1)),
            Self::Range { from, to } => snapshot.refs_between(*from, *to),
        }
    }

    fn boundaries(self) -> (usize, usize) {
        match self {
            Self::AggregateThrough(index) => (0, index.saturating_add(1)),
            Self::Layer(index) => (index, index.saturating_add(1)),
            Self::Range { from, to } => (from, to),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StackReviewTimeFilter {
    All,
    Days(u16),
}

impl StackReviewTimeFilter {
    fn cutoff(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::Days(days) => {
                Some(OffsetDateTime::now_utc().unix_timestamp() - i64::from(days) * 24 * 60 * 60)
            }
        }
    }
}

#[derive(Clone)]
struct StackReviewFileItem {
    path: SharedString,
    fingerprint: SharedString,
    provenance: StackReviewFileProvenance,
    content_kind: StackReviewContentKind,
}

fn is_test_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    if path
        .split('/')
        .any(|component| matches!(component, "test" | "tests" | "__tests__" | "test-data"))
    {
        return true;
    }
    let file_name = path.rsplit('/').next().unwrap_or(path.as_str());
    if file_name.contains(".test.") || file_name.contains(".it-test.") {
        return true;
    }
    file_name
        .rsplit_once(".spec.")
        .is_some_and(|(_, extension)| {
            matches!(
                extension,
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "py" | "rs" | "go"
            )
        })
}

fn is_migration_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let components = path.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .take(components.len().saturating_sub(1))
        .any(|component| *component == "migrations")
    {
        return true;
    }
    let Some((stem, extension)) = components
        .last()
        .and_then(|file_name| file_name.rsplit_once('.'))
    else {
        return false;
    };
    matches!(extension, "sql" | "ts" | "js")
        && (stem == "migration"
            || stem.ends_with(".migration")
            || stem.ends_with("_migration")
            || stem.ends_with("-migration"))
}

fn is_visible_review_path(path: &str, hide_tests: bool, hide_migrations: bool) -> bool {
    !(hide_tests && is_test_path(path) || hide_migrations && is_migration_path(path))
}

fn stack_review_file_fingerprint(file: &git::stack_review::StackReviewFileDiff) -> SharedString {
    let identity = format!(
        "{}\0{:?}\0{:?}\0{:?}",
        file.path, file.status, file.old_content, file.new_content
    );
    Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes())
        .to_string()
        .into()
}

impl StackReviewFileItem {
    fn provenance_label(&self) -> Option<&'static str> {
        match self.provenance {
            StackReviewFileProvenance::Direct => None,
            StackReviewFileProvenance::Merge => Some("Merge touched"),
            StackReviewFileProvenance::Mixed => Some("Direct + merge touched"),
            StackReviewFileProvenance::Unknown => Some("Unknown origin"),
        }
    }

    fn content_label(&self) -> Option<&'static str> {
        match self.content_kind {
            StackReviewContentKind::Text => None,
            StackReviewContentKind::Binary => Some("Binary"),
            StackReviewContentKind::NonBlob => Some("Git object"),
            StackReviewContentKind::Unavailable => Some("Unavailable"),
        }
    }
}

fn display_revision_content(content: Option<RevisionContent>) -> String {
    match content {
        Some(RevisionContent::Text(text)) => text,
        Some(RevisionContent::Binary) => "Binary file; content not shown\n".to_owned(),
        Some(RevisionContent::NonBlob(object_type)) => {
            format!("{object_type} Git object; content not shown\n")
        }
        Some(RevisionContent::Unavailable(reason)) => {
            format!("Content unavailable locally: {reason}\n")
        }
        None => String::new(),
    }
}

fn display_texts_for_file(
    old_content: Option<RevisionContent>,
    new_content: Option<RevisionContent>,
) -> (String, String) {
    (
        display_revision_content(old_content),
        display_revision_content(new_content),
    )
}

async fn build_active_diff_view(
    entry: Option<ContentDiffEntry>,
    comments: Vec<git::stack_review::StackReviewComment>,
    project: Entity<Project>,
    workspace: gpui::WeakEntity<Workspace>,
    cx: &mut AsyncWindowContext,
) -> Result<(
    Entity<MultiDiffView>,
    Vec<git::stack_review::StackReviewComment>,
)> {
    let entries = entry.into_iter().collect();
    let workspace_entity = workspace
        .upgrade()
        .context("Stack Review workspace no longer exists")?;
    let build_task = cx.update(|window, cx| {
        MultiDiffView::build_from_content(entries, project, workspace_entity, window, cx)
    })?;
    let diff_view = build_task.await?;
    let restored_comments = cx.update(|window, cx| {
        diff_view.read(cx).editor().update(cx, |editor, cx| {
            editor.restore_stack_review_comments(&comments, cx);
            editor.reveal_restored_stack_review_comments(window, cx);
            editor.stack_review_comments(cx)
        })
    })?;
    workspace.update_in(cx, |workspace, window, cx| {
        diff_view.update(cx, |diff_view, cx| {
            diff_view.added_to_workspace(workspace, window, cx);
        });
    })?;
    Ok((diff_view, restored_comments))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LoadedCommentRecord {
    path: PathBuf,
    record: StackReviewCommentRecord,
    serialized: String,
}

struct CommentProjection {
    comments: Vec<StackReviewComment>,
    record_id_by_editor_id: HashMap<usize, String>,
}

enum CommentWrite {
    Upsert {
        path: PathBuf,
        expected: Option<String>,
        serialized: String,
    },
    Delete {
        path: PathBuf,
        expected: String,
    },
}

async fn apply_comment_writes(
    fs: Arc<dyn Fs>,
    write_lock: Arc<futures::lock::Mutex<()>>,
    writes: Vec<CommentWrite>,
) -> Result<()> {
    let _guard = write_lock.lock().await;
    for write in writes {
        match write {
            CommentWrite::Upsert {
                path,
                expected,
                serialized,
            } => {
                match expected {
                    Some(expected) => anyhow::ensure!(
                        fs.load(&path).await? == expected,
                        "comment changed on disk before Zed could save {path:?}"
                    ),
                    None => anyhow::ensure!(
                        !fs.is_file(&path).await,
                        "comment file already exists at {path:?}"
                    ),
                }
                fs.atomic_write(path, serialized).await?;
            }
            CommentWrite::Delete { path, expected } => {
                anyhow::ensure!(
                    fs.load(&path).await? == expected,
                    "comment changed on disk before Zed could delete {path:?}"
                );
                fs.remove_file(
                    &path,
                    RemoveOptions {
                        ignore_if_not_exists: true,
                        ..Default::default()
                    },
                )
                .await?;
            }
        }
    }
    Ok(())
}

fn stack_review_storage_key(base_oid: &str, head_oid: &str) -> String {
    format!("{base_oid}-{head_oid}")
        .replace(|character: char| !character.is_ascii_alphanumeric(), "_")
}

fn stack_review_timestamp() -> String {
    OffsetDateTime::now_utc().to_string()
}

fn comment_file_name(record: &StackReviewCommentRecord) -> String {
    let prefix = match record.source {
        StackReviewCommentSource::LocalHuman => "local",
        StackReviewCommentSource::LocalAgent => "agent",
        StackReviewCommentSource::Github => "github",
    };
    format!("{prefix}-{}.json", record.id)
}

async fn load_comment_directory(
    fs: &Arc<dyn Fs>,
    directory: &Path,
    base_oid: &str,
    head_oid: &str,
) -> Result<HashMap<String, LoadedCommentRecord>> {
    if !fs.is_dir(directory).await {
        return Ok(HashMap::new());
    }
    let mut entries = fs.read_dir(directory).await?;
    let mut records = HashMap::new();
    while let Some(path) = entries.next().await {
        let path = path?;
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let serialized = fs.load(&path).await?;
        let record = StackReviewCommentRecord::from_json(&serialized, base_oid, head_oid)
            .with_context(|| format!("reading Stack Review comment {path:?}"))?;
        anyhow::ensure!(
            records
                .insert(
                    record.id.clone(),
                    LoadedCommentRecord {
                        path,
                        record: record.clone(),
                        serialized,
                    },
                )
                .is_none(),
            "duplicate Stack Review comment id {:?}",
            record.id
        );
    }
    Ok(records)
}

async fn load_all_comment_records(
    fs: &Arc<dyn Fs>,
    comments_directory: &Path,
    github_comments_directory: &Path,
    base_oid: &str,
    head_oid: &str,
) -> Result<HashMap<String, LoadedCommentRecord>> {
    let mut records = load_comment_directory(fs, comments_directory, base_oid, head_oid).await?;
    for (id, record) in
        load_comment_directory(fs, github_comments_directory, base_oid, head_oid).await?
    {
        anyhow::ensure!(
            records.insert(id.clone(), record).is_none(),
            "duplicate local/GitHub comment id {id:?}"
        );
    }
    Ok(records)
}

fn project_comment_records(
    records: &HashMap<String, LoadedCommentRecord>,
    include_resolved: bool,
) -> CommentProjection {
    let mut inline_records = records
        .values()
        .filter(|loaded| {
            loaded.record.side == StackReviewCommentSide::Right
                && !loaded.record.outdated
                && (include_resolved || !loaded.record.is_resolved())
        })
        .collect::<Vec<_>>();
    inline_records.sort_by(|left, right| {
        left.record
            .created_at
            .cmp(&right.record.created_at)
            .then_with(|| left.record.id.cmp(&right.record.id))
    });
    let editor_id_by_record_id = inline_records
        .iter()
        .enumerate()
        .map(|(editor_id, loaded)| (loaded.record.id.clone(), editor_id))
        .collect::<HashMap<_, _>>();
    let mut record_id_by_editor_id = HashMap::new();
    let comments = inline_records
        .into_iter()
        .enumerate()
        .map(|(editor_id, loaded)| {
            record_id_by_editor_id.insert(editor_id, loaded.record.id.clone());
            StackReviewComment {
                id: editor_id,
                path: loaded.record.path.clone().unwrap_or_default(),
                start_row: loaded.record.start_row.unwrap_or_default(),
                start_column: loaded.record.start_column.unwrap_or_default(),
                end_row: loaded.record.end_row.unwrap_or_default(),
                end_column: loaded.record.end_column.unwrap_or_default(),
                body: loaded.record.body.clone(),
                created_at: loaded.record.created_at.clone(),
                resolved: loaded.record.is_resolved(),
                author: loaded.record.author.clone(),
                source: loaded.record.source,
                reply_to: loaded
                    .record
                    .reply_to
                    .as_ref()
                    .and_then(|reply_to| editor_id_by_record_id.get(reply_to).copied()),
            }
        })
        .collect();
    CommentProjection {
        comments,
        record_id_by_editor_id,
    }
}

async fn migrate_legacy_comments(
    fs: &Arc<dyn Fs>,
    comments_directory: &Path,
    base_oid: &str,
    head_oid: &str,
    legacy_comments: Vec<StackReviewComment>,
    records: &mut HashMap<String, LoadedCommentRecord>,
) -> Result<()> {
    if legacy_comments.is_empty() {
        return Ok(());
    }
    let record_ids = legacy_comments
        .iter()
        .map(|comment| {
            let identity = format!("{base_oid}\0{head_oid}\0{}\0{}", comment.path, comment.id);
            (
                comment.id,
                Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()).to_string(),
            )
        })
        .collect::<HashMap<_, _>>();
    let timestamp = stack_review_timestamp();
    for comment in legacy_comments {
        let id = record_ids
            .get(&comment.id)
            .context("legacy comment id was not assigned")?
            .clone();
        if records.contains_key(&id) {
            continue;
        }
        let record = StackReviewCommentRecord::new_inline(
            id.clone(),
            base_oid.to_owned(),
            head_oid.to_owned(),
            comment.path,
            comment.start_row,
            comment.start_column,
            comment.end_row,
            comment.end_column,
            comment.body,
            comment.author,
            comment.source,
            comment
                .reply_to
                .and_then(|reply_to| record_ids.get(&reply_to).cloned()),
            timestamp.clone(),
        );
        let serialized = record.to_json()?;
        let path = comments_directory.join(comment_file_name(&record));
        fs.atomic_write(path.clone(), serialized.clone()).await?;
        records.insert(
            id,
            LoadedCommentRecord {
                path,
                record,
                serialized,
            },
        );
    }
    Ok(())
}

struct LoadedStackReview {
    diff_view: Entity<MultiDiffView>,
    provenance_summary: SharedString,
    review_state: StackReviewState,
    review_state_path: Option<PathBuf>,
    state_error: Option<SharedString>,
    files: Vec<StackReviewFileItem>,
    content_entries: Vec<ContentDiffEntry>,
    selected_file_index: Option<usize>,
    rendered_comment_ids: HashSet<usize>,
    comment_records: HashMap<String, LoadedCommentRecord>,
    record_id_by_editor_id: HashMap<usize, String>,
    comments_directory: PathBuf,
    github_comments_directory: PathBuf,
}

pub struct StackReview {
    snapshot: StackSnapshot,
    current_layer: usize,
    selected_scope: StackReviewScope,
    time_filter: StackReviewTimeFilter,
    custom_days_editor: Entity<Editor>,
    has_worktree_changes: bool,
    diverged_layer_count: usize,
    repository: Entity<Repository>,
    project: Entity<Project>,
    workspace: gpui::WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    state_root: PathBuf,
    work_directory: PathBuf,
    review_state: Option<StackReviewState>,
    review_state_path: Option<PathBuf>,
    files: Vec<StackReviewFileItem>,
    content_entries: Vec<ContentDiffEntry>,
    selected_file_index: Option<usize>,
    hide_tests: bool,
    hide_migrations: bool,
    show_resolved_comments: bool,
    sidebar_width: Pixels,
    review_comment_count: usize,
    rendered_comment_ids: HashSet<usize>,
    comment_records: HashMap<String, LoadedCommentRecord>,
    record_id_by_editor_id: HashMap<usize, String>,
    comments_directory: Option<PathBuf>,
    github_comments_directory: Option<PathBuf>,
    diff_view: Option<Entity<MultiDiffView>>,
    provenance_summary: Option<SharedString>,
    state_error: Option<SharedString>,
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    nav_history: Option<ItemNavHistory>,
    load_task: Task<()>,
    file_load_task: Task<()>,
    state_write_lock: Arc<futures::lock::Mutex<()>>,
    comment_write_lock: Arc<futures::lock::Mutex<()>>,
    state_write_generations: HashMap<PathBuf, Arc<AtomicU64>>,
    editor_subscription: Option<Subscription>,
    comment_watch_task: Task<()>,
}

impl StackReview {
    pub(crate) fn register(workspace: &mut Workspace, _cx: &mut Context<Workspace>) {
        workspace.register_action(Self::deploy);
    }

    fn deploy(
        workspace: &mut Workspace,
        _: &ReviewStack,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        if !cx.has_flag::<StackReviewFeatureFlag>() {
            Self::notify_error(
                "Enable the stack-review feature flag to use Stack Review",
                window,
                cx,
            );
            return;
        }
        let project = workspace.project().clone();
        let fs = project.read(cx).fs().clone();
        let Some(repository) = project.read(cx).active_repository(cx) else {
            Self::notify_error("No active Git repository", window, cx);
            return;
        };
        let repository_snapshot = repository.read(cx).snapshot();
        let Some(current_branch) = repository_snapshot.branch.as_ref() else {
            Self::notify_error(
                "Stack review requires a checked-out local branch",
                window,
                cx,
            );
            return;
        };
        let current_branch = current_branch.name().to_owned();
        let has_worktree_changes = repository_snapshot.status().next().is_some();
        let branch_heads = repository_snapshot
            .branch_list
            .iter()
            .filter_map(|branch| {
                Some((
                    branch.name().to_owned(),
                    branch.most_recent_commit.as_ref()?.sha.to_string(),
                ))
            })
            .collect::<HashMap<_, _>>();
        let common_dir = repository_snapshot.common_dir_abs_path.clone();
        let work_directory = repository_snapshot.work_directory_abs_path.to_path_buf();
        let state_root = common_dir.join("zed-stack-review");
        let metadata_common_dir = common_dir;
        let read_metadata = cx.background_executor().spawn(async move {
            let gh_stack_path = metadata_common_dir.join("gh-stack");
            match std::fs::read_to_string(&gh_stack_path) {
                Ok(contents) => Ok(Some(contents)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let explicit_path = metadata_common_dir.join("zed-stack-review/stack.json");
                    match std::fs::read_to_string(&explicit_path) {
                        Ok(contents) => Ok(Some(contents)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(error).with_context(|| {
                            format!("reading local stack metadata at {explicit_path:?}")
                        }),
                    }
                }
                Err(error) => Err(error)
                    .with_context(|| format!("reading local stack metadata at {gh_stack_path:?}")),
            }
        });
        let workspace_entity = cx.entity();
        let workspace_weak = workspace_entity.downgrade();
        window
            .spawn(cx, async move |cx| {
                let (stack_file, discovered) = if let Some(contents) = read_metadata.await? {
                    (parse_stack_file(&contents)?, false)
                } else {
                    match discover_stack_with_github_cli(&work_directory, &current_branch).await {
                        Ok(stack_file) => (stack_file, true),
                        Err(error) => {
                            prompt_to_configure_github_cli(&error, cx).await?;
                            return Err(error.into());
                        }
                    }
                };
                if discovered {
                    fs.create_dir(&state_root).await?;
                    fs.atomic_write(state_root.join("stack.json"), stack_file.to_json()?)
                        .await?;
                }
                let stack = stack_file.stack_for_branch(&current_branch)?;
                let snapshot = stack.resolve(&branch_heads)?;
                let ancestry = repository.update(cx, |repository, _| {
                    repository.inspect_stack_ancestry(snapshot.clone())
                });
                let diverged_layer_count = ancestry
                    .await??
                    .into_iter()
                    .filter(|layer| !layer.is_ancestor)
                    .count();
                let current_layer = snapshot
                    .layers
                    .iter()
                    .position(|layer| layer.head.branch == current_branch)
                    .context("current branch is the stack trunk; select a stack branch")?;

                workspace_entity.update_in(cx, |workspace, window, cx| {
                    let existing = workspace.items_of_type::<Self>(cx).find(|review| {
                        review.read(cx).snapshot == snapshot
                            && review.read(cx).current_layer == current_layer
                    });
                    if let Some(existing) = existing {
                        existing.update(cx, |review, cx| {
                            review.load_scope(
                                StackReviewScope::AggregateThrough(current_layer),
                                window,
                                cx,
                            );
                        });
                        workspace.activate_item(&existing, true, true, window, cx);
                        return;
                    }
                    let workspace_handle = cx.entity().downgrade();
                    let review = cx.new(|cx| {
                        Self::new(
                            snapshot,
                            current_layer,
                            has_worktree_changes,
                            diverged_layer_count,
                            repository,
                            project,
                            workspace_handle,
                            fs,
                            state_root,
                            work_directory,
                            window,
                            cx,
                        )
                    });
                    review.update(cx, |review, cx| {
                        review.load_scope(
                            StackReviewScope::AggregateThrough(current_layer),
                            window,
                            cx,
                        );
                    });
                    workspace.add_item_to_active_pane(Box::new(review), None, true, window, cx);
                })?;
                anyhow::Ok(())
            })
            .detach_and_notify_err(workspace_weak, window, cx);
    }

    fn notify_error(message: &'static str, window: &mut Window, cx: &mut Context<Workspace>) {
        let workspace = cx.entity().downgrade();
        window
            .spawn(cx, async move |_cx| Err::<(), _>(anyhow!(message)))
            .detach_and_notify_err(workspace, window, cx);
    }

    fn new(
        snapshot: StackSnapshot,
        current_layer: usize,
        has_worktree_changes: bool,
        diverged_layer_count: usize,
        repository: Entity<Repository>,
        project: Entity<Project>,
        workspace: gpui::WeakEntity<Workspace>,
        fs: Arc<dyn Fs>,
        state_root: PathBuf,
        work_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let custom_days_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Days", window, cx);
            editor
        });
        Self {
            snapshot,
            current_layer,
            selected_scope: StackReviewScope::AggregateThrough(current_layer),
            time_filter: StackReviewTimeFilter::All,
            custom_days_editor,
            has_worktree_changes,
            diverged_layer_count,
            repository,
            project,
            workspace,
            fs,
            state_root,
            work_directory,
            review_state: None,
            review_state_path: None,
            files: Vec::new(),
            content_entries: Vec::new(),
            selected_file_index: None,
            hide_tests: false,
            hide_migrations: false,
            show_resolved_comments: false,
            sidebar_width: STACK_REVIEW_SIDEBAR_DEFAULT_WIDTH,
            review_comment_count: 0,
            rendered_comment_ids: HashSet::new(),
            comment_records: HashMap::new(),
            record_id_by_editor_id: HashMap::new(),
            comments_directory: None,
            github_comments_directory: None,
            diff_view: None,
            provenance_summary: None,
            state_error: None,
            error: None,
            focus_handle: cx.focus_handle(),
            nav_history: None,
            load_task: Task::ready(()),
            file_load_task: Task::ready(()),
            state_write_lock: Arc::new(futures::lock::Mutex::new(())),
            comment_write_lock: Arc::new(futures::lock::Mutex::new(())),
            state_write_generations: HashMap::new(),
            editor_subscription: None,
            comment_watch_task: Task::ready(()),
        }
    }

    fn load_scope(&mut self, scope: StackReviewScope, window: &mut Window, cx: &mut Context<Self>) {
        let Some((base_ref, head_ref)) = scope.refs(&self.snapshot) else {
            self.error = Some("Selected stack layer no longer exists".into());
            cx.notify();
            return;
        };
        log::info!(
            "[STACK_REVIEW_DEBUG] load scope: scope={scope:?}, base={base_ref}, head={head_ref}, time_filter={:?}",
            self.time_filter
        );
        self.selected_scope = scope;
        self.file_load_task = Task::ready(());
        self.diff_view = None;
        self.provenance_summary = None;
        self.review_state = None;
        self.review_state_path = None;
        self.files.clear();
        self.content_entries.clear();
        self.selected_file_index = None;
        self.review_comment_count = 0;
        self.rendered_comment_ids.clear();
        self.comment_records.clear();
        self.record_id_by_editor_id.clear();
        self.comments_directory = None;
        self.github_comments_directory = None;
        self.comment_watch_task = Task::ready(());
        self.state_error = None;
        self.editor_subscription = None;
        self.error = None;
        cx.notify();

        let base_ref = base_ref.to_owned();
        let head_ref = head_ref.to_owned();
        let cutoff = self.time_filter.cutoff();
        let receiver = self.repository.update(cx, |repository, _| {
            if let Some(cutoff) = cutoff {
                repository.stack_review_diff_since(base_ref, head_ref, cutoff)
            } else {
                repository.stack_review_diff(base_ref, head_ref)
            }
        });
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        let fs = self.fs.clone();
        let state_root = self.state_root.clone();
        let work_directory = self.work_directory.clone();
        let show_resolved_comments = self.show_resolved_comments;
        let hide_tests = self.hide_tests;
        let hide_migrations = self.hide_migrations;
        let layer_pull_requests: Vec<(u32, String)> = match scope {
            StackReviewScope::Layer(index) => self
                .snapshot
                .layers
                .get(index)
                .and_then(|layer| {
                    layer
                        .head
                        .pull_request_number
                        .map(|number| (number, layer.head.oid.clone()))
                })
                .into_iter()
                .collect(),
            StackReviewScope::AggregateThrough(index) => self
                .snapshot
                .layers
                .iter()
                .take(index.saturating_add(1))
                .filter_map(|layer| {
                    layer
                        .head
                        .pull_request_number
                        .map(|number| (number, layer.head.oid.clone()))
                })
                .collect(),
            StackReviewScope::Range { from, to } => self
                .snapshot
                .layers
                .iter()
                .skip(from)
                .take(to.saturating_sub(from))
                .filter_map(|layer| {
                    layer
                        .head
                        .pull_request_number
                        .map(|number| (number, layer.head.oid.clone()))
                })
                .collect(),
        };
        self.load_task = cx.spawn_in(window, async move |this, cx| {
            let result: Result<LoadedStackReview> = async {
                let diff = receiver.await??;
                let counts = diff.provenance_counts();
                let mut provenance_parts = Vec::new();
                if counts.merge > 0 {
                    provenance_parts.push(format!("{} merge-touched", counts.merge));
                }
                if counts.mixed > 0 {
                    provenance_parts.push(format!("{} direct + merge-touched", counts.mixed));
                }
                if counts.unknown > 0 {
                    provenance_parts.push(format!("{} unknown origin", counts.unknown));
                }
                let provenance_summary: SharedString = provenance_parts.join(" · ").into();
                let storage_key = stack_review_storage_key(&diff.base_ref, &diff.head_ref);
                let review_state_path = state_root
                    .join("reviews")
                    .join(format!("{storage_key}.json"));
                let comments_directory = state_root.join("comments").join(&storage_key);
                let github_comments_directory = state_root.join("github").join(&storage_key);
                fs.create_dir(
                    review_state_path
                        .parent()
                        .context("review-state path has no parent")?,
                )
                .await?;
                fs.create_dir(&comments_directory).await?;
                fs.create_dir(&github_comments_directory).await?;
                let (mut review_state, state_error, writable_review_state_path) = if fs
                    .is_file(&review_state_path)
                    .await
                {
                    match fs
                        .load(&review_state_path)
                        .await
                        .and_then(|contents| StackReviewState::from_json(&contents))
                    {
                        Ok(state) if state.matches_snapshot(&diff.base_ref, &diff.head_ref) => {
                            (state, None, Some(review_state_path.clone()))
                        }
                        Ok(_) => (
                            StackReviewState::new(&diff.base_ref, &diff.head_ref),
                            Some("Review state does not match the selected Git snapshot".into()),
                            None,
                        ),
                        Err(error) => (
                            StackReviewState::new(&diff.base_ref, &diff.head_ref),
                            Some(format!("Unable to load review state: {error}").into()),
                            None,
                        ),
                    }
                } else {
                    (
                        StackReviewState::new(&diff.base_ref, &diff.head_ref),
                        None,
                        Some(review_state_path.clone()),
                    )
                };
                if let Err(error) = refresh_github_comment_projection(
                    &fs,
                    &work_directory,
                    &github_comments_directory,
                    &diff.base_ref,
                    &diff.head_ref,
                    &layer_pull_requests,
                )
                .await
                {
                    log::warn!(
                        "unable to refresh GitHub comments; using local projection: {error:#}"
                    );
                }
                let mut comment_records = load_all_comment_records(
                    &fs,
                    &comments_directory,
                    &github_comments_directory,
                    &diff.base_ref,
                    &diff.head_ref,
                )
                .await?;
                if let Some(writable_review_state_path) = &writable_review_state_path {
                    let legacy_comments = review_state.take_comments();
                    migrate_legacy_comments(
                        &fs,
                        &comments_directory,
                        &diff.base_ref,
                        &diff.head_ref,
                        legacy_comments,
                        &mut comment_records,
                    )
                    .await?;
                    fs.atomic_write(writable_review_state_path.clone(), review_state.to_json()?)
                        .await?;
                }
                let manifest = StackReviewCurrentManifest::new(
                    work_directory.to_string_lossy().into_owned(),
                    diff.base_ref.clone(),
                    diff.head_ref.clone(),
                    format!("reviews/{storage_key}.json"),
                    format!("comments/{storage_key}"),
                    format!("github/{storage_key}"),
                    layer_pull_requests
                        .iter()
                        .map(|(number, _)| *number)
                        .collect(),
                );
                fs.atomic_write(state_root.join("current.json"), manifest.to_json()?)
                    .await?;
                let comment_projection =
                    project_comment_records(&comment_records, show_resolved_comments);
                let files: Vec<StackReviewFileItem> = diff
                    .files
                    .iter()
                    .map(|file| StackReviewFileItem {
                        path: file.path.clone().into(),
                        fingerprint: stack_review_file_fingerprint(file),
                        provenance: file.provenance,
                        content_kind: file.content_kind,
                    })
                    .collect();
                let content_entries: Vec<ContentDiffEntry> = diff
                    .files
                    .into_iter()
                    .map(|file| {
                        let path = PathBuf::from(file.path);
                        let has_text_content =
                            matches!(file.old_content.as_ref(), Some(RevisionContent::Text(_)))
                                || matches!(
                                    file.new_content.as_ref(),
                                    Some(RevisionContent::Text(_))
                                );
                        let (old_text, new_text) =
                            display_texts_for_file(file.old_content, file.new_content);
                        ContentDiffEntry {
                            source_path: has_text_content.then(|| work_directory.join(&path)),
                            was_deleted: file.status == StackReviewFileStatus::Deleted,
                            path,
                            old_text,
                            new_text,
                        }
                    })
                    .collect();
                let selected_file_index = files.iter().position(|file| {
                    is_visible_review_path(&file.path, hide_tests, hide_migrations)
                });
                let active_entry = selected_file_index
                    .and_then(|index| content_entries.get(index))
                    .cloned();
                let (diff_view, restored_comments) = build_active_diff_view(
                    active_entry,
                    comment_projection.comments,
                    project,
                    workspace,
                    cx,
                )
                .await?;
                let rendered_comment_ids =
                    restored_comments.iter().map(|comment| comment.id).collect();
                Ok(LoadedStackReview {
                    diff_view,
                    provenance_summary,
                    review_state,
                    review_state_path: writable_review_state_path,
                    state_error,
                    files,
                    content_entries,
                    selected_file_index,
                    rendered_comment_ids,
                    comment_records,
                    record_id_by_editor_id: comment_projection.record_id_by_editor_id,
                    comments_directory,
                    github_comments_directory,
                })
            }
            .await;

            if let Err(error) = this.update_in(cx, |this, window, cx| match result {
                Ok(loaded) => {
                    let editor = loaded.diff_view.read(cx).editor();
                    let selected_file_index = loaded.selected_file_index;
                    let active_path = loaded
                        .selected_file_index
                        .and_then(|index| loaded.content_entries.get(index))
                        .map(|entry| entry.path.to_string_lossy().into_owned());
                    let review_comment_count = loaded.comment_records.len();
                    if let Some(nav_history) = this.nav_history.clone() {
                        editor.update(cx, |editor, _| {
                            editor.set_nav_history(Some(nav_history));
                        });
                    }
                    this.diff_view = selected_file_index.map(|_| loaded.diff_view);
                    this.provenance_summary = Some(loaded.provenance_summary);
                    this.review_state = Some(loaded.review_state);
                    this.review_state_path = loaded.review_state_path;
                    this.files = loaded.files;
                    this.content_entries = loaded.content_entries;
                    this.selected_file_index = selected_file_index;
                    this.review_comment_count = review_comment_count;
                    this.rendered_comment_ids = loaded.rendered_comment_ids;
                    this.comment_records = loaded.comment_records;
                    this.record_id_by_editor_id = loaded.record_id_by_editor_id;
                    this.comments_directory = Some(loaded.comments_directory);
                    this.github_comments_directory = Some(loaded.github_comments_directory);
                    log::info!(
                        "[STACK_REVIEW_DEBUG] scope loaded: files={}, comments={}, selected={:?}",
                        this.files.len(),
                        this.review_comment_count,
                        this.selected_file_index
                    );
                    this.editor_subscription = active_path.map(|active_path| {
                        cx.subscribe_in(
                            &editor,
                            window,
                            move |this, editor, event: &EditorEvent, window, cx| {
                                cx.emit(event.clone());
                                match event {
                                    EditorEvent::ReviewCommentsChanged { .. } => {
                                        let comments = editor.read(cx).stack_review_comments(cx);
                                        this.reconcile_editor_comments(&active_path, comments, cx);
                                    }
                                    EditorEvent::ReviewCommentResolutionChanged {
                                        ids,
                                        resolved,
                                    } => {
                                        this.persist_comment_resolution(ids, *resolved, window, cx);
                                    }
                                    _ => {}
                                }
                            },
                        )
                    });
                    this.state_error = loaded.state_error;
                    this.error = None;
                    this.start_comment_watch(window, cx);
                    window.focus(&editor.focus_handle(cx), cx);
                    cx.notify();
                }
                Err(error) => {
                    this.diff_view = None;
                    this.provenance_summary = None;
                    this.review_state = None;
                    this.review_state_path = None;
                    this.files.clear();
                    this.review_comment_count = 0;
                    this.state_error = None;
                    this.error = Some(error.to_string().into());
                    cx.notify();
                }
            }) {
                log::error!("failed to update stack review after loading diff: {error:#}");
            }
        });
    }

    fn select_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.content_entries.get(index).cloned() else {
            return;
        };
        let active_path = entry.path.to_string_lossy().into_owned();
        let comment_projection =
            project_comment_records(&self.comment_records, self.show_resolved_comments);
        let comments = comment_projection.comments;
        self.record_id_by_editor_id = comment_projection.record_id_by_editor_id;
        self.selected_file_index = Some(index);
        self.diff_view = None;
        self.editor_subscription = None;
        self.rendered_comment_ids.clear();
        self.error = None;
        cx.notify();

        let project = self.project.clone();
        let workspace = self.workspace.clone();
        self.file_load_task = cx.spawn_in(window, async move |this, cx| {
            let result =
                build_active_diff_view(Some(entry), comments, project, workspace, cx).await;
            if let Err(error) = this.update_in(cx, |this, window, cx| match result {
                Ok((diff_view, restored_comments)) => {
                    let editor = diff_view.read(cx).editor();
                    if let Some(nav_history) = this.nav_history.clone() {
                        editor.update(cx, |editor, _| {
                            editor.set_nav_history(Some(nav_history));
                        });
                    }
                    this.rendered_comment_ids =
                        restored_comments.iter().map(|comment| comment.id).collect();
                    this.diff_view = Some(diff_view);
                    this.editor_subscription = Some(cx.subscribe_in(
                        &editor,
                        window,
                        move |this, editor, event: &EditorEvent, window, cx| {
                            cx.emit(event.clone());
                            match event {
                                EditorEvent::ReviewCommentsChanged { .. } => {
                                    let comments = editor.read(cx).stack_review_comments(cx);
                                    this.reconcile_editor_comments(&active_path, comments, cx);
                                }
                                EditorEvent::ReviewCommentResolutionChanged { ids, resolved } => {
                                    this.persist_comment_resolution(ids, *resolved, window, cx);
                                }
                                _ => {}
                            }
                        },
                    ));
                    this.error = None;
                    window.focus(&editor.focus_handle(cx), cx);
                    cx.notify();
                }
                Err(error) => {
                    this.diff_view = None;
                    this.error = Some(error.to_string().into());
                    cx.notify();
                }
            }) {
                log::error!("failed to update active stack-review file: {error:#}");
            }
        });
    }

    fn start_comment_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comments_directory) = self.comments_directory.clone() else {
            return;
        };
        let Some(github_comments_directory) = self.github_comments_directory.clone() else {
            return;
        };
        let Some(review_state) = self.review_state.as_ref() else {
            return;
        };
        let base_oid = review_state.base_oid.clone();
        let head_oid = review_state.head_oid.clone();
        let fs = self.fs.clone();
        self.comment_watch_task = cx.spawn_in(window, async move |this, cx| {
            let (local_events, _local_watcher) = fs
                .watch(&comments_directory, Duration::from_millis(250))
                .await;
            let (github_events, _github_watcher) = fs
                .watch(&github_comments_directory, Duration::from_millis(250))
                .await;
            let mut events = futures::stream::select(local_events, github_events);
            while events.next().await.is_some() {
                let records = load_all_comment_records(
                    &fs,
                    &comments_directory,
                    &github_comments_directory,
                    &base_oid,
                    &head_oid,
                )
                .await;
                if let Err(update_error) = this.update_in(cx, |this, _window, cx| match records {
                    Ok(records) if records != this.comment_records => {
                        log::debug!(
                            "[STACK_REVIEW_DEBUG] comment files changed: old={}, new={}",
                            this.comment_records.len(),
                            records.len()
                        );
                        this.comment_records = records;
                        this.review_comment_count = this.comment_records.len();
                        this.record_id_by_editor_id = project_comment_records(
                            &this.comment_records,
                            this.show_resolved_comments,
                        )
                        .record_id_by_editor_id;
                        this.state_error = Some(
                            "Comments changed on disk; switch files to refresh the active diff"
                                .into(),
                        );
                        cx.notify();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.state_error = Some(error.to_string().into());
                        cx.notify();
                    }
                }) {
                    log::error!("failed to reload Stack Review comments: {update_error:#}");
                    break;
                }
            }
        });
    }

    fn persist_comment_resolution(
        &mut self,
        editor_ids: &[usize],
        resolved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut writes = Vec::new();
        for editor_id in editor_ids {
            let Some(record_id) = self.record_id_by_editor_id.get(editor_id).cloned() else {
                continue;
            };
            let Some(loaded) = self.comment_records.get_mut(&record_id) else {
                continue;
            };
            let expected = loaded.serialized.clone();
            let mut record = loaded.record.clone();
            if record.source == StackReviewCommentSource::Github {
                record.local_resolution = Some(resolved);
            } else {
                record.resolved = resolved;
            }
            record.updated_at = stack_review_timestamp();
            let Ok(serialized) = record.to_json() else {
                self.state_error = Some("Unable to serialize comment resolution".into());
                cx.notify();
                return;
            };
            writes.push(CommentWrite::Upsert {
                path: loaded.path.clone(),
                expected: Some(expected),
                serialized: serialized.clone(),
            });
            loaded.record = record;
            loaded.serialized = serialized;
        }
        if writes.is_empty() {
            return;
        }
        let fs = self.fs.clone();
        let write_lock = self.comment_write_lock.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = apply_comment_writes(fs, write_lock, writes).await;
            if let Err(update_error) = this.update(cx, |this, cx| {
                this.state_error = result.err().map(|error| error.to_string().into());
                cx.notify();
            }) {
                log::error!("failed to report comment resolution write: {update_error:#}");
            }
        })
        .detach();
        if let Some(index) = self.selected_file_index {
            self.select_file(index, window, cx);
        }
    }

    fn reconcile_editor_comments(
        &mut self,
        active_path: &str,
        comments: Vec<StackReviewComment>,
        cx: &mut Context<Self>,
    ) {
        let Some(comments_directory) = self.comments_directory.clone() else {
            return;
        };
        let Some(review_state) = self.review_state.as_ref() else {
            return;
        };
        for comment in &comments {
            self.record_id_by_editor_id
                .entry(comment.id)
                .or_insert_with(|| Uuid::now_v7().to_string());
        }
        let current_editor_ids = comments
            .iter()
            .map(|comment| comment.id)
            .collect::<HashSet<_>>();
        let mut writes = Vec::new();
        for editor_id in self
            .rendered_comment_ids
            .difference(&current_editor_ids)
            .copied()
            .collect::<Vec<_>>()
        {
            let Some(record_id) = self.record_id_by_editor_id.remove(&editor_id) else {
                continue;
            };
            let Some(loaded) = self.comment_records.get(&record_id) else {
                continue;
            };
            if !loaded.record.is_writable() {
                continue;
            }
            let Some(loaded) = self.comment_records.remove(&record_id) else {
                continue;
            };
            writes.push(CommentWrite::Delete {
                path: loaded.path,
                expected: loaded.serialized,
            });
        }
        let timestamp = stack_review_timestamp();
        for comment in comments {
            let record_id = self.record_id_by_editor_id[&comment.id].clone();
            let reply_to = comment
                .reply_to
                .and_then(|reply_to| self.record_id_by_editor_id.get(&reply_to).cloned());
            let existing = self.comment_records.get(&record_id).cloned();
            if existing
                .as_ref()
                .is_some_and(|loaded| !loaded.record.is_writable())
            {
                continue;
            }
            let mut record = StackReviewCommentRecord::new_inline(
                record_id.clone(),
                review_state.base_oid.clone(),
                review_state.head_oid.clone(),
                active_path.to_owned(),
                comment.start_row,
                comment.start_column,
                comment.end_row,
                comment.end_column,
                comment.body,
                comment.author,
                comment.source,
                reply_to,
                timestamp.clone(),
            );
            if let Some(existing) = &existing {
                record.created_at = existing.record.created_at.clone();
                record.updated_at = existing.record.updated_at.clone();
                record.outdated = existing.record.outdated;
                record.resolved = existing.record.resolved;
                record.local_resolution = existing.record.local_resolution;
                if record == existing.record {
                    continue;
                }
                record.updated_at = timestamp.clone();
            }
            let serialized = match record.to_json() {
                Ok(serialized) => serialized,
                Err(error) => {
                    self.state_error = Some(error.to_string().into());
                    cx.notify();
                    return;
                }
            };
            let path = existing
                .as_ref()
                .map(|existing| existing.path.clone())
                .unwrap_or_else(|| comments_directory.join(comment_file_name(&record)));
            writes.push(CommentWrite::Upsert {
                path: path.clone(),
                expected: existing
                    .as_ref()
                    .map(|existing| existing.serialized.clone()),
                serialized: serialized.clone(),
            });
            self.comment_records.insert(
                record_id,
                LoadedCommentRecord {
                    path,
                    record,
                    serialized,
                },
            );
        }
        self.rendered_comment_ids = current_editor_ids;
        self.review_comment_count = self.comment_records.len();
        if writes.is_empty() {
            return;
        }
        let fs = self.fs.clone();
        let write_lock = self.comment_write_lock.clone();
        cx.spawn(async move |this, cx| {
            let result = apply_comment_writes(fs, write_lock, writes).await;
            if let Err(update_error) = this.update(cx, |this, cx| {
                this.state_error = result.err().map(|error| error.to_string().into());
                cx.notify();
            }) {
                log::error!("failed to report Stack Review comment write: {update_error:#}");
            }
        })
        .detach();
    }

    fn add_comment_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff_view) = &self.diff_view {
            diff_view.read(cx).editor().update(cx, |editor, cx| {
                editor.show_stack_review_comment_at_cursor(window, cx);
            });
        }
    }

    fn toggle_file_reviewed(
        &mut self,
        path: SharedString,
        fingerprint: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(review_state) = self.review_state.as_mut() else {
            return;
        };
        let reviewed = !review_state.is_file_reviewed(&path, &fingerprint);
        review_state.set_file_reviewed(path.to_string(), fingerprint.to_string(), reviewed);
        self.queue_state_write(cx);
    }

    fn queue_state_write(&mut self, cx: &mut Context<Self>) {
        let Some(review_state) = self.review_state.as_ref() else {
            return;
        };
        let Some(review_state_path) = self.review_state_path.clone() else {
            return;
        };
        let contents = match review_state.to_json() {
            Ok(contents) => contents,
            Err(error) => {
                self.state_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        self.state_error = None;
        cx.notify();

        let fs = self.fs.clone();
        let write_lock = self.state_write_lock.clone();
        let latest_generation = self
            .state_write_generations
            .entry(review_state_path.clone())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        let generation = latest_generation.fetch_add(1, Ordering::AcqRel) + 1;
        cx.spawn(async move |this, cx| {
            let _guard = write_lock.lock().await;
            if latest_generation.load(Ordering::Acquire) != generation {
                return;
            }
            let result = async {
                let parent = review_state_path
                    .parent()
                    .context("review-state path has no parent directory")?;
                fs.create_dir(parent).await?;
                fs.atomic_write(review_state_path.clone(), contents).await
            }
            .await;
            if let Err(update_error) = this.update(cx, |this, cx| {
                if this.review_state_path.as_ref() == Some(&review_state_path) {
                    this.state_error = result.err().map(|error| error.to_string().into());
                    cx.notify();
                }
            }) {
                log::error!("failed to report stack-review state write: {update_error:#}");
            }
        })
        .detach();
    }

    fn title(&self) -> SharedString {
        let tip = self
            .snapshot
            .layers
            .get(self.current_layer)
            .map(|layer| layer.head.branch.as_str())
            .unwrap_or("stack");
        format!("Stack Review: {tip}").into()
    }

    fn set_time_filter(
        &mut self,
        time_filter: StackReviewTimeFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.time_filter = time_filter;
        self.load_scope(self.selected_scope, window, cx);
    }

    fn apply_custom_days(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.custom_days_editor.read(cx).text(cx);
        let days = value.trim().parse::<u16>();
        match days {
            Ok(days @ 1..=3650) => {
                self.state_error = None;
                self.set_time_filter(StackReviewTimeFilter::Days(days), window, cx);
            }
            _ => {
                self.state_error = Some("Custom days must be between 1 and 3650".into());
                cx.notify();
            }
        }
    }

    fn visible_file_indexes(&self) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                is_visible_review_path(&file.path, self.hide_tests, self.hide_migrations)
                    .then_some(index)
            })
            .collect()
    }

    fn select_adjacent_file(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let visible_indexes = self.visible_file_indexes();
        let Some(current_position) = self
            .selected_file_index
            .and_then(|selected| visible_indexes.iter().position(|index| *index == selected))
        else {
            if let Some(index) = visible_indexes.first().copied() {
                self.select_file(index, window, cx);
            }
            return;
        };
        let next_position = if forward {
            current_position
                .saturating_add(1)
                .min(visible_indexes.len().saturating_sub(1))
        } else {
            current_position.saturating_sub(1)
        };
        if let Some(index) = visible_indexes.get(next_position).copied() {
            self.select_file(index, window, cx);
        }
    }

    fn next_file(&mut self, _: &StackReviewNextFile, window: &mut Window, cx: &mut Context<Self>) {
        self.select_adjacent_file(true, window, cx);
    }

    fn previous_file(
        &mut self,
        _: &StackReviewPreviousFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_adjacent_file(false, window, cx);
    }

    fn reconcile_filtered_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible_indexes = self.visible_file_indexes();
        if self
            .selected_file_index
            .is_some_and(|selected| visible_indexes.contains(&selected))
        {
            cx.notify();
            return;
        }
        if let Some(index) = visible_indexes.first().copied() {
            self.select_file(index, window, cx);
        } else {
            self.file_load_task = Task::ready(());
            self.selected_file_index = None;
            self.diff_view = None;
            self.editor_subscription = None;
            self.rendered_comment_ids.clear();
            cx.notify();
        }
    }

    fn toggle_test_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_tests = !self.hide_tests;
        self.reconcile_filtered_selection(window, cx);
    }

    fn toggle_migration_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_migrations = !self.hide_migrations;
        self.reconcile_filtered_selection(window, cx);
    }

    fn toggle_resolved_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_resolved_comments = !self.show_resolved_comments;
        if let Some(index) = self.selected_file_index {
            self.select_file(index, window, cx);
        } else {
            cx.notify();
        }
    }

    fn render_file_item(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let file = self.files.get(index)?.clone();
        let path = file.path.clone();
        let fingerprint = file.fingerprint.clone();
        let detail: Option<SharedString> = match (file.provenance_label(), file.content_label()) {
            (Some(provenance), Some(content)) => Some(format!("{provenance} · {content}").into()),
            (Some(provenance), None) => Some(provenance.into()),
            (None, Some(content)) => Some(content.into()),
            (None, None) => None,
        };
        let has_detail = detail.is_some();
        let reviewed = self
            .review_state
            .as_ref()
            .is_some_and(|state| state.is_file_reviewed(&path, &fingerprint));
        let selected = self.selected_file_index == Some(index);
        Some(
            ListItem::new(("stack-review-file-row", index))
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .aria_label(file.path.clone())
                .start_slot(
                    Checkbox::new(("stack-review-file", index), reviewed.into()).on_click(
                        cx.listener(move |this, _, _window, cx| {
                            this.toggle_file_reviewed(path.clone(), fingerprint.clone(), cx);
                        }),
                    ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_file(index, window, cx);
                }))
                .child(
                    v_flex()
                        .min_w_0()
                        .h(px(36.))
                        .when(!has_detail, |row| row.justify_center())
                        .debug_selector(move || format!("STACK_REVIEW_FILE-{index}"))
                        .child(Label::new(file.path).truncate())
                        .when_some(detail, |row, detail| {
                            row.child(Label::new(detail).color(Color::Muted))
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let visible_indexes = Arc::new(self.visible_file_indexes());
        let reviewed_count = self
            .review_state
            .as_ref()
            .map(StackReviewState::reviewed_file_count)
            .unwrap_or_default();
        let total_file_count = self.files.len();
        let visible_file_count = visible_indexes.len();
        let test_count = self
            .files
            .iter()
            .filter(|file| is_test_path(&file.path))
            .count();
        let migration_count = self
            .files
            .iter()
            .filter(|file| is_migration_path(&file.path))
            .count();
        v_flex()
            .id("stack-review-files")
            .debug_selector(|| "STACK_REVIEW_FILE_SIDEBAR".to_owned())
            .relative()
            .w(self.sidebar_width)
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(format!(
                        "{reviewed_count}/{total_file_count} reviewed · {visible_file_count} shown"
                    )))
                    .child(
                        h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                Button::new(
                                    "stack-review-filter-tests",
                                    if self.hide_tests {
                                        format!("Show Tests ({test_count})")
                                    } else {
                                        format!("Hide Tests ({test_count})")
                                    },
                                )
                                .toggle_state(self.hide_tests)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.toggle_test_filter(window, cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(
                                    "stack-review-filter-migrations",
                                    if self.hide_migrations {
                                        format!("Show Migrations ({migration_count})")
                                    } else {
                                        format!("Hide Migrations ({migration_count})")
                                    },
                                )
                                .toggle_state(self.hide_migrations)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.toggle_migration_filter(window, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(
                uniform_list(
                    "stack-review-file-list",
                    visible_file_count,
                    cx.processor({
                        move |this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .filter_map(|visible_index| {
                                    visible_indexes
                                        .get(visible_index)
                                        .and_then(|index| this.render_file_item(*index, cx))
                                })
                                .collect()
                        }
                    }),
                )
                .flex_1(),
            )
            .child(deferred(
                div()
                    .id("stack-review-sidebar-resize")
                    .debug_selector(|| "STACK_REVIEW_SIDEBAR_RESIZE".to_owned())
                    .absolute()
                    .right(px(-2.))
                    .top_0()
                    .h_full()
                    .w(px(5.))
                    .cursor_col_resize()
                    .on_drag(DraggedStackReviewSidebar, |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| gpui::Empty)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .occlude(),
            ))
    }

    fn render_preserved_comments(&self) -> Option<AnyElement> {
        let count = self
            .comment_records
            .values()
            .filter(|loaded| {
                loaded.record.side != StackReviewCommentSide::Right || loaded.record.outdated
            })
            .count();
        (count > 0).then(|| {
            Label::new(format!("{count} PR comments / outdated threads"))
                .size(ui::LabelSize::Small)
                .color(Color::Muted)
                .into_any_element()
        })
    }

    fn boundary_label(&self, index: usize) -> SharedString {
        self.snapshot
            .boundary(index)
            .map(|boundary| {
                let short_oid = boundary.oid.get(..8).unwrap_or(&boundary.oid);
                if let Some(pull_request_number) = boundary.pull_request_number {
                    format!(
                        "PR #{pull_request_number} · {} · {short_oid}",
                        boundary.branch
                    )
                    .into()
                } else {
                    format!("Trunk · {} · {short_oid}", boundary.branch).into()
                }
            })
            .unwrap_or_else(|| "Missing boundary".into())
    }

    fn set_boundary_range(
        &mut self,
        from: usize,
        to: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot.refs_between(from, to).is_none() {
            self.state_error = Some("The From boundary must precede the To boundary".into());
            cx.notify();
            return;
        }
        self.state_error = None;
        self.load_scope(StackReviewScope::Range { from, to }, window, cx);
    }

    fn render_boundary_dropdown(
        &self,
        select_from: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DropdownMenu {
        let (from, to) = self.selected_scope.boundaries();
        let selected = if select_from { from } else { to };
        let choices = if select_from {
            (0..to)
                .map(|index| (index, self.boundary_label(index)))
                .collect::<Vec<_>>()
        } else {
            (from.saturating_add(1)..=self.snapshot.layers.len())
                .map(|index| (index, self.boundary_label(index)))
                .collect::<Vec<_>>()
        };
        let selected_position = choices.iter().position(|(index, _)| *index == selected);
        let weak = cx.weak_entity();
        DropdownMenu::new(
            if select_from {
                "stack-review-from"
            } else {
                "stack-review-to"
            },
            self.boundary_label(selected),
            ContextMenu::build(window, cx, move |mut menu, window, cx| {
                for (index, label) in &choices {
                    let index = *index;
                    let weak = weak.clone();
                    let label = label.clone();
                    menu = menu.entry(label, None, move |window, cx| {
                        if let Err(error) = weak.update(cx, |this, cx| {
                            let (current_from, current_to) = this.selected_scope.boundaries();
                            let (from, to) = if select_from {
                                (index, current_to)
                            } else {
                                (current_from, index)
                            };
                            this.set_boundary_range(from, to, window, cx);
                        }) {
                            log::error!("unable to update Stack Review boundaries: {error:#}");
                        }
                    });
                }
                if let Some(selected_position) = selected_position {
                    for _ in 0..=selected_position {
                        menu.select_next(&Default::default(), window, cx);
                    }
                }
                menu
            }),
        )
    }

    fn render_header(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let aggregate_scope = StackReviewScope::AggregateThrough(self.current_layer);
        let current_scope = StackReviewScope::Layer(self.current_layer);
        let from_dropdown = self.render_boundary_dropdown(true, window, cx);
        let to_dropdown = self.render_boundary_dropdown(false, window, cx);
        let shortcut_focus = self.focus_handle(cx);
        let shortcuts = DropdownMenu::new(
            "stack-review-shortcuts",
            "Shortcuts",
            ContextMenu::build(window, cx, move |menu, _, _| {
                menu.context(shortcut_focus)
                    .action("Previous file", Box::new(StackReviewPreviousFile))
                    .action("Next file", Box::new(StackReviewNextFile))
                    .separator()
                    .action(
                        "Previous changed hunk",
                        Box::new(editor::actions::GoToPreviousChange),
                    )
                    .action(
                        "Next changed hunk",
                        Box::new(editor::actions::GoToNextChange),
                    )
                    .action(
                        "Resolve or reopen thread at cursor",
                        Box::new(editor::actions::ToggleActiveReviewCommentResolved),
                    )
                    .separator()
                    .action("Show or hide tests", Box::new(StackReviewToggleTests))
                    .action(
                        "Show or hide migrations",
                        Box::new(StackReviewToggleMigrations),
                    )
            }),
        );
        let resolved_comment_count = self
            .comment_records
            .values()
            .filter(|loaded| loaded.record.is_resolved())
            .count();
        let status = h_flex()
            .w_full()
            .flex_wrap()
            .gap_1()
            .child(
                Label::new(if self.has_worktree_changes {
                    "Committed only · local WIP excluded"
                } else {
                    "Committed only"
                })
                .color(Color::Muted),
            )
            .when(self.diverged_layer_count > 0, |status| {
                status.child(
                    Label::new(format!(
                        "{} diverged layers · merge-base diff",
                        self.diverged_layer_count
                    ))
                    .color(Color::Warning),
                )
            })
            .when_some(
                self.provenance_summary
                    .clone()
                    .filter(|summary| !summary.is_empty()),
                |status, summary| status.child(Label::new(summary).color(Color::Muted)),
            )
            .child(
                Label::new(format!("{} comments", self.review_comment_count)).color(Color::Muted),
            )
            .when(resolved_comment_count > 0, |status| {
                status.child(
                    Button::new(
                        "stack-review-show-resolved",
                        if self.show_resolved_comments {
                            format!("Hide Resolved ({resolved_comment_count})")
                        } else {
                            format!("Show Resolved ({resolved_comment_count})")
                        },
                    )
                    .toggle_state(self.show_resolved_comments)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_resolved_comments(window, cx);
                    })),
                )
            })
            .when(self.time_filter != StackReviewTimeFilter::All, |status| {
                status.child(Label::new("File filter · full file diffs shown").color(Color::Muted))
            })
            .child(
                Button::new("stack-review-add-comment", "Add Comment")
                    .disabled(self.diff_view.is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_comment_at_cursor(window, cx);
                    })),
            )
            .when_some(self.state_error.clone(), |status, error| {
                status.child(Label::new(error).color(Color::Error))
            });

        let mut time_controls = h_flex()
            .w_full()
            .gap_1()
            .child(Label::new("Files touched by author time").color(Color::Muted));
        for (index, (time_filter, label)) in [
            (StackReviewTimeFilter::All, "All"),
            (StackReviewTimeFilter::Days(1), "24h"),
            (StackReviewTimeFilter::Days(3), "3d"),
            (StackReviewTimeFilter::Days(7), "7d"),
            (StackReviewTimeFilter::Days(30), "30d"),
        ]
        .into_iter()
        .enumerate()
        {
            time_controls = time_controls.child(
                Button::new(("stack-review-time", index), label)
                    .toggle_state(self.time_filter == time_filter)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_time_filter(time_filter, window, cx);
                    })),
            );
        }
        time_controls = time_controls
            .child(
                div()
                    .w(px(72.))
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded_md()
                    .px_1()
                    .child(self.custom_days_editor.clone()),
            )
            .child(
                Button::new("stack-review-custom-days", "Apply").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.apply_custom_days(window, cx);
                    },
                )),
            );

        let scope_controls = h_flex()
            .id("stack-review-scopes")
            .w_full()
            .flex_wrap()
            .gap_1()
            .child(Label::new("From").color(Color::Muted))
            .child(
                div()
                    .debug_selector(|| "STACK_REVIEW_FROM_BOUNDARY".to_owned())
                    .child(from_dropdown),
            )
            .child(Label::new("To").color(Color::Muted))
            .child(
                div()
                    .debug_selector(|| "STACK_REVIEW_TO_BOUNDARY".to_owned())
                    .child(to_dropdown),
            )
            .child(shortcuts)
            .child(
                Button::new("stack-review-aggregate", "Whole Stack")
                    .toggle_state(self.selected_scope.boundaries() == aggregate_scope.boundaries())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.load_scope(aggregate_scope, window, cx);
                    })),
            )
            .child(
                Button::new("stack-review-current", "Current PR")
                    .toggle_state(self.selected_scope.boundaries() == current_scope.boundaries())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.load_scope(current_scope, window, cx);
                    })),
            );

        v_flex()
            .w_full()
            .flex_none()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(status)
            .child(time_controls)
            .child(scope_controls)
            .when_some(self.render_preserved_comments(), |header, comments| {
                header.child(comments)
            })
    }
}

impl EventEmitter<EditorEvent> for StackReview {}

impl Focusable for StackReview {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.diff_view
            .as_ref()
            .map(|diff_view| diff_view.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl Item for StackReview {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::GitBranch).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        Label::new(self.title())
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(self.title())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Stack Review Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff_view) = &self.diff_view {
            diff_view.update(cx, |diff_view, cx| {
                diff_view.deactivated(window, cx);
            });
        }
    }

    fn as_searchable(
        &self,
        _handle: &Entity<Self>,
        cx: &App,
    ) -> Option<Box<dyn SearchableItemHandle>> {
        self.diff_view
            .as_ref()
            .map(|view| view.read(cx).searchable_handle())
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nav_history = Some(nav_history.clone());
        if let Some(diff_view) = &self.diff_view {
            diff_view
                .read(cx)
                .editor()
                .update(cx, |editor, _| editor.set_nav_history(Some(nav_history)));
        }
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.diff_view.as_ref().is_some_and(|view| {
            view.read(cx)
                .editor()
                .update(cx, |editor, cx| editor.navigate(data, window, cx))
        })
    }

    fn to_item_events(event: &EditorEvent, emit: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, emit)
    }
}

impl Render for StackReview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(window, cx);
        let file_list = self.render_file_list(cx);
        let all_files_hidden = !self.files.is_empty() && self.visible_file_indexes().is_empty();
        v_flex()
            .size_full()
            .key_context("StackReview")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::previous_file))
            .on_action(cx.listener(|this, _: &StackReviewToggleTests, window, cx| {
                this.toggle_test_filter(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &StackReviewToggleMigrations, window, cx| {
                    this.toggle_migration_filter(window, cx);
                }),
            )
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<DraggedStackReviewSidebar>, _window, cx| {
                    this.sidebar_width = (event.event.position.x - event.bounds.left()).clamp(
                        STACK_REVIEW_SIDEBAR_MIN_WIDTH,
                        STACK_REVIEW_SIDEBAR_MAX_WIDTH,
                    );
                    cx.notify();
                },
            ))
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .overflow_hidden()
                    .when(!self.files.is_empty(), |body| body.child(file_list))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .overflow_hidden()
                            .debug_selector(|| "STACK_REVIEW_DIFF_PANEL".into())
                            .when_some(self.error.clone(), |element, error| {
                                element.child(
                                    v_flex()
                                        .size_full()
                                        .items_center()
                                        .justify_center()
                                        .child(Label::new(error).color(Color::Error)),
                                )
                            })
                            .when(
                                self.error.is_none()
                                    && self.diff_view.is_none()
                                    && !all_files_hidden,
                                |element| {
                                    element.child(
                                        v_flex().size_full().items_center().justify_center().child(
                                            Label::new("Loading stack diff…").color(Color::Muted),
                                        ),
                                    )
                                },
                            )
                            .when(all_files_hidden, |element| {
                                element.child(
                                    v_flex().size_full().items_center().justify_center().child(
                                        Label::new(
                                            "All changed files are hidden by the current filters.",
                                        )
                                        .color(Color::Muted),
                                    ),
                                )
                            })
                            .when_some(self.diff_view.clone(), |element, diff_view| {
                                element.child(div().size_full().child(diff_view))
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Modifiers, MouseDownEvent, MouseMoveEvent, MouseUpEvent, TestAppContext, VisualTestContext,
        point,
    };
    use language::language_settings::AllLanguageSettings;
    use project::{FakeFs, WorktreeSettings, project_settings::ProjectSettings};
    use serde_json::json;
    use settings::{Settings as _, SettingsStore};
    use theme::LoadThemes;
    use workspace::WorkspaceSettings;

    fn init_test(cx: &mut TestAppContext) {
        zlog::init_test();
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(LoadThemes::JustBase, cx);
            AllLanguageSettings::register(cx);
            editor::init(cx);
            ProjectSettings::register(cx);
            WorktreeSettings::register(cx);
            WorkspaceSettings::register(cx);
        });
    }

    #[gpui::test]
    fn stack_review_default_keybindings_load(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            for path in [
                "keymaps/default-macos.json",
                "keymaps/default-linux.json",
                "keymaps/default-windows.json",
            ] {
                let bindings = settings::KeymapFile::load_asset_allow_partial_failure(path, cx)
                    .unwrap_or_else(|error| panic!("failed to load {path}: {error:#}"));
                for action_name in [
                    "git::StackReviewPreviousFile",
                    "git::StackReviewNextFile",
                    "editor::ToggleActiveReviewCommentResolved",
                    "git::StackReviewToggleTests",
                    "git::StackReviewToggleMigrations",
                ] {
                    assert!(
                        bindings
                            .iter()
                            .any(|binding| binding.action().name() == action_name),
                        "{path} did not load {action_name}"
                    );
                }
            }
        });
    }

    #[test]
    fn maps_all_github_comment_classes_to_read_only_records() {
        let records = github_comment_records(
            42,
            "base",
            "head",
            vec![json!({
                "id": 10,
                "path": "src/lib.rs",
                "line": 5,
                "side": "RIGHT",
                "body": "Inline",
                "user": { "login": "octocat" },
                "html_url": "https://github.test/inline",
                "created_at": "2026-08-21T12:00:00Z",
                "commit_id": "abc"
            })],
            vec![json!({
                "id": 11,
                "body": "Review summary",
                "user": { "login": "reviewer" },
                "html_url": "https://github.test/review",
                "submitted_at": "2026-08-21T12:01:00Z",
                "commit_id": "abc"
            })],
            vec![json!({
                "id": 12,
                "body": "Conversation",
                "user": { "login": "participant" },
                "html_url": "https://github.test/conversation",
                "created_at": "2026-08-21T12:02:00Z"
            })],
            &HashMap::from([(10, (true, false))]),
            false,
        )
        .expect("map GitHub comments");

        assert_eq!(records.len(), 3);
        assert_eq!(records[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(records[0].start_row, Some(4));
        assert!(!records[0].is_writable());
        assert!(records[0].resolved);
        assert_eq!(records[1].side, StackReviewCommentSide::TopLevel);
        assert_eq!(records[2].side, StackReviewCommentSide::TopLevel);
    }

    #[gpui::test]
    async fn per_comment_store_migrates_and_detects_external_conflicts(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        let comments_directory = Path::new("/repo/.git/zed-stack-review/comments/base-head");
        fs.create_dir(comments_directory)
            .await
            .expect("create comment directory");
        let mut records = HashMap::new();
        migrate_legacy_comments(
            &(fs.clone() as Arc<dyn Fs>),
            comments_directory,
            "base",
            "head",
            vec![StackReviewComment {
                id: 7,
                path: "src/lib.rs".into(),
                start_row: 4,
                start_column: 0,
                end_row: 4,
                end_column: 2,
                body: "Migrated".into(),
                created_at: String::new(),
                resolved: false,
                author: git::stack_review::StackReviewCommentAuthor::default(),
                source: StackReviewCommentSource::LocalHuman,
                reply_to: None,
            }],
            &mut records,
        )
        .await
        .expect("migrate legacy comment");
        assert_eq!(records.len(), 1);
        let projection = project_comment_records(&records, false);
        assert_eq!(projection.comments.len(), 1);
        assert_eq!(projection.comments[0].body, "Migrated");
        assert!(!projection.comments[0].created_at.is_empty());
        assert!(!projection.comments[0].resolved);
        let mut resolved_records = records.clone();
        resolved_records
            .values_mut()
            .next()
            .expect("resolved record")
            .record
            .local_resolution = Some(true);
        assert!(
            project_comment_records(&resolved_records, false)
                .comments
                .is_empty()
        );
        assert_eq!(
            project_comment_records(&resolved_records, true)
                .comments
                .len(),
            1
        );
        let loaded = records.values().next().expect("migrated record").clone();

        let agent_record = StackReviewCommentRecord::new_inline(
            Uuid::now_v7().to_string(),
            "base".into(),
            "head".into(),
            "src/agent.rs".into(),
            2,
            0,
            2,
            1,
            "Agent-authored".into(),
            StackReviewCommentAuthor {
                name: "Claude Code".into(),
                login: None,
            },
            StackReviewCommentSource::LocalAgent,
            None,
            stack_review_timestamp(),
        );
        fs.atomic_write(
            comments_directory.join(comment_file_name(&agent_record)),
            agent_record.to_json().expect("serialize agent record"),
        )
        .await
        .expect("external agent writes comment");
        let agent_records = load_comment_directory(
            &(fs.clone() as Arc<dyn Fs>),
            comments_directory,
            "base",
            "head",
        )
        .await
        .expect("reload external agent comments");
        assert!(agent_records.values().any(|loaded| {
            loaded.record.body == "Agent-authored"
                && loaded.record.source == StackReviewCommentSource::LocalAgent
        }));

        fs.atomic_write(loaded.path.clone(), "external edit".into())
            .await
            .expect("external agent edit");
        let error = apply_comment_writes(
            fs,
            Arc::new(futures::lock::Mutex::new(())),
            vec![CommentWrite::Upsert {
                path: loaded.path,
                expected: Some(loaded.serialized),
                serialized: loaded.record.to_json().expect("serialize record"),
            }],
        )
        .await
        .expect_err("stale Zed write must not overwrite an external edit");
        assert!(error.to_string().contains("changed on disk"));
    }

    #[test]
    fn display_texts_preserve_each_revision_side_independently() {
        let (old_text, new_text) = display_texts_for_file(
            Some(git::repository::RevisionContent::Binary),
            Some(git::repository::RevisionContent::Text("now text\n".into())),
        );

        assert!(old_text.contains("Binary file"));
        assert_eq!(new_text, "now text\n");

        let (old_text, new_text) = display_texts_for_file(
            Some(git::repository::RevisionContent::Text("was text\n".into())),
            Some(git::repository::RevisionContent::Unavailable(
                "missing object".into(),
            )),
        );
        assert_eq!(old_text, "was text\n");
        assert!(new_text.contains("missing object"));
    }

    #[test]
    fn classifies_review_noise_without_hiding_behavior_specs() {
        assert!(is_test_path(
            "apps/api/src/routes/file-parsing/file-parsing.service.test.ts"
        ));
        assert!(is_test_path(
            "apps/frontend/tests/e2e/upload/upload-measurements.spec.ts"
        ));
        assert!(is_test_path("test-data/pdf/fixtures/report.pdf"));
        assert!(!is_test_path(
            "apps/api/src/routes/file-parsing/file-parsing.service.spec.md"
        ));
        assert!(is_migration_path(
            "apps/api/supabase/migrations/20260820210139_change.sql"
        ));
        assert!(is_migration_path(
            "apps/api/supabase/migrations/meta/_journal.json"
        ));
        assert!(is_migration_path("db/user.migration.ts"));
        assert!(!is_migration_path(
            "apps/api/src/services/migration-helper.ts"
        ));
        assert!(!is_migration_path("drizzle/schema.ts"));
        assert!(!is_visible_review_path("src/service.test.ts", true, false));
        assert!(is_visible_review_path("src/service.test.ts", false, false));
    }

    #[test]
    fn converts_github_pr_relationships_to_a_stack() {
        let stack_file = stack_file_from_github_prs(
            "feature/ui",
            vec![
                GitHubPullRequest {
                    number: 12,
                    head_ref_name: "feature/ui".into(),
                    base_ref_name: "feature/models".into(),
                },
                GitHubPullRequest {
                    number: 11,
                    head_ref_name: "feature/models".into(),
                    base_ref_name: "staging".into(),
                },
                GitHubPullRequest {
                    number: 10,
                    head_ref_name: "staging".into(),
                    base_ref_name: "main".into(),
                },
            ],
        )
        .expect("valid PR chain");

        let stack = &stack_file.stacks[0];
        assert_eq!(stack.trunk.branch, "staging");
        assert_eq!(
            stack
                .branches
                .iter()
                .map(|branch| branch.branch.as_str())
                .collect::<Vec<_>>(),
            ["feature/models", "feature/ui"]
        );
        assert_eq!(
            stack
                .branches
                .iter()
                .map(|branch| branch.pull_request.as_ref().map(|pr| pr.number))
                .collect::<Vec<_>>(),
            [Some(11), Some(12)]
        );
    }

    #[gpui::test]
    async fn clicking_a_sidebar_filename_selects_that_file(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            "/project",
            json!({
                ".git": {},
                "first.rs": "fn first() {}",
                "second.rs": "fn second() {}",
                "third.rs": "fn third() {}"
            }),
        )
        .await;
        fs.set_head_and_index_for_repo(
            Path::new("/project/.git"),
            &[
                ("first.rs", "fn first() {}".to_owned()),
                ("second.rs", "fn second() {}".to_owned()),
                ("third.rs", "fn third() {}".to_owned()),
            ],
        );
        let project = Project::test(fs.clone(), [Path::new("/project")], cx).await;
        let workspace =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut visual_context = VisualTestContext::from_window(*workspace, cx);
        let build_task = workspace
            .update(&mut visual_context, |_workspace, window, cx| {
                MultiDiffView::build_from_content(
                    vec![ContentDiffEntry {
                        path: PathBuf::from("first.rs"),
                        source_path: Some(PathBuf::from("/project/first.rs")),
                        was_deleted: false,
                        old_text: "fn old_first() {}".into(),
                        new_text: "fn first() {}".into(),
                    }],
                    project.clone(),
                    cx.entity(),
                    window,
                    cx,
                )
            })
            .expect("build diff task");
        let diff_view = build_task.await.expect("build diff view");
        let review = workspace
            .update(&mut visual_context, |workspace, window, cx| {
                let repository = project
                    .read(cx)
                    .active_repository(cx)
                    .expect("active repository");
                let workspace_handle = cx.entity().downgrade();
                let snapshot = StackSnapshot {
                    number: None,
                    trunk: git::stack_review::ResolvedStackBranch {
                        branch: "staging".into(),
                        oid: "base".into(),
                        pull_request_number: None,
                    },
                    layers: vec![git::stack_review::ResolvedStackLayer {
                        base: git::stack_review::ResolvedStackBranch {
                            branch: "staging".into(),
                            oid: "base".into(),
                            pull_request_number: None,
                        },
                        head: git::stack_review::ResolvedStackBranch {
                            branch: "feature".into(),
                            oid: "head".into(),
                            pull_request_number: Some(1),
                        },
                    }],
                };
                let review = cx.new(|cx| StackReview {
                    snapshot,
                    current_layer: 0,
                    selected_scope: StackReviewScope::AggregateThrough(0),
                    time_filter: StackReviewTimeFilter::All,
                    custom_days_editor: cx.new(|cx| Editor::single_line(window, cx)),
                    has_worktree_changes: false,
                    diverged_layer_count: 0,
                    repository,
                    project: project.clone(),
                    workspace: workspace_handle,
                    fs: fs.clone(),
                    state_root: PathBuf::from("/project/.git/zed-stack-review"),
                    work_directory: PathBuf::from("/project"),
                    review_state: Some(StackReviewState::new("base", "head")),
                    review_state_path: None,
                    files: vec![
                        StackReviewFileItem {
                            path: "first.rs".into(),
                            fingerprint: "first-fingerprint".into(),
                            provenance: StackReviewFileProvenance::Direct,
                            content_kind: StackReviewContentKind::Text,
                        },
                        StackReviewFileItem {
                            path: "second.rs".into(),
                            fingerprint: "second-fingerprint".into(),
                            provenance: StackReviewFileProvenance::Direct,
                            content_kind: StackReviewContentKind::Binary,
                        },
                        StackReviewFileItem {
                            path: "third.rs".into(),
                            fingerprint: "third-fingerprint".into(),
                            provenance: StackReviewFileProvenance::Direct,
                            content_kind: StackReviewContentKind::Text,
                        },
                    ],
                    content_entries: vec![
                        ContentDiffEntry {
                            path: PathBuf::from("first.rs"),
                            source_path: Some(PathBuf::from("/project/first.rs")),
                            was_deleted: false,
                            old_text: "fn old_first() {}".into(),
                            new_text: "fn first() {}".into(),
                        },
                        ContentDiffEntry {
                            path: PathBuf::from("second.rs"),
                            source_path: Some(PathBuf::from("/project/second.rs")),
                            was_deleted: false,
                            old_text: "fn old_second() {}".into(),
                            new_text: "fn second() {}".into(),
                        },
                        ContentDiffEntry {
                            path: PathBuf::from("third.rs"),
                            source_path: Some(PathBuf::from("/project/third.rs")),
                            was_deleted: false,
                            old_text: "fn old_third() {}".into(),
                            new_text: "fn third() {}".into(),
                        },
                    ],
                    selected_file_index: Some(0),
                    hide_tests: false,
                    hide_migrations: false,
                    show_resolved_comments: false,
                    sidebar_width: STACK_REVIEW_SIDEBAR_DEFAULT_WIDTH,
                    review_comment_count: 0,
                    rendered_comment_ids: HashSet::new(),
                    comment_records: HashMap::new(),
                    record_id_by_editor_id: HashMap::new(),
                    comments_directory: None,
                    github_comments_directory: None,
                    diff_view: Some(diff_view.clone()),
                    provenance_summary: None,
                    state_error: None,
                    error: None,
                    focus_handle: cx.focus_handle(),
                    nav_history: None,
                    load_task: Task::ready(()),
                    file_load_task: Task::ready(()),
                    state_write_lock: Arc::new(futures::lock::Mutex::new(())),
                    comment_write_lock: Arc::new(futures::lock::Mutex::new(())),
                    state_write_generations: HashMap::new(),
                    editor_subscription: None,
                    comment_watch_task: Task::ready(()),
                });
                workspace.add_item_to_active_pane(Box::new(review.clone()), None, true, window, cx);
                review
            })
            .expect("update workspace");
        visual_context.run_until_parked();
        let initial_sidebar_bounds = visual_context
            .debug_bounds("STACK_REVIEW_FILE_SIDEBAR")
            .expect("file sidebar bounds");
        let resize_bounds = visual_context
            .debug_bounds("STACK_REVIEW_SIDEBAR_RESIZE")
            .expect("sidebar resize handle bounds");
        let resized_position = point(
            initial_sidebar_bounds.left() + px(420.),
            resize_bounds.center().y,
        );
        visual_context.simulate_event(MouseDownEvent {
            position: resize_bounds.center(),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        });
        visual_context.run_until_parked();
        visual_context.simulate_event(MouseMoveEvent {
            position: point(resize_bounds.center().x + px(10.), resize_bounds.center().y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        visual_context.run_until_parked();
        visual_context.simulate_event(MouseMoveEvent {
            position: resized_position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::none(),
        });
        visual_context.run_until_parked();
        visual_context.simulate_event(MouseUpEvent {
            position: resized_position,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
            click_count: 1,
        });
        visual_context.run_until_parked();
        let expanded_sidebar_bounds = visual_context
            .debug_bounds("STACK_REVIEW_FILE_SIDEBAR")
            .expect("expanded file sidebar bounds");
        assert!(expanded_sidebar_bounds.size.width > initial_sidebar_bounds.size.width);
        assert!(
            visual_context
                .debug_bounds("STACK_REVIEW_FROM_BOUNDARY")
                .is_some(),
            "From boundary dropdown must render"
        );
        assert!(
            visual_context
                .debug_bounds("STACK_REVIEW_TO_BOUNDARY")
                .is_some(),
            "To boundary dropdown must render"
        );
        let bounds = visual_context
            .debug_bounds("STACK_REVIEW_FILE-1")
            .expect("second file target bounds");
        let third_bounds = visual_context
            .debug_bounds("STACK_REVIEW_FILE-2")
            .expect("third file target bounds");
        assert!(
            bounds.bottom() <= third_bounds.top(),
            "sidebar file rows overlap"
        );
        let diff_bounds = visual_context
            .debug_bounds("STACK_REVIEW_DIFF_PANEL")
            .expect("diff panel bounds");
        assert!(diff_bounds.size.width > px(0.));
        assert!(diff_bounds.size.height > px(0.));

        visual_context.simulate_click(bounds.center(), Modifiers::none());

        assert_eq!(
            review.read_with(&visual_context, |review, _| review.selected_file_index),
            Some(1)
        );
        let selected_diff_view = review
            .read_with(&visual_context, |review, _| review.diff_view.clone())
            .expect("selected diff view");
        assert_eq!(
            review.read_with(&visual_context, |review, cx| review.focus_handle(cx)),
            selected_diff_view.read_with(&visual_context, |diff_view, cx| {
                diff_view.focus_handle(cx)
            }),
            "Stack Review must delegate focus to the active diff editor"
        );
        let selected_path = selected_diff_view.update(&mut visual_context, |diff_view, cx| {
            diff_view.editor().update(cx, |editor, cx| {
                let snapshot = editor.display_snapshot(cx);
                let point = editor
                    .selections
                    .newest::<language::Point>(&snapshot)
                    .head();
                editor
                    .review_file_path_at(point, cx)
                    .map(|path| path.as_unix_str().to_owned())
            })
        });
        assert_eq!(selected_path.as_deref(), Some("second.rs"));
    }
}
