pub(super) mod blame;

use super::*;
use ::git::{
    Restore,
    blame::BlameEntry,
    commit::ParsedCommitMessage,
    stack_review::{StackReviewComment, StackReviewCommentAuthor, StackReviewCommentSource},
    status::FileStatus,
};
use buffer_diff::{BufferDiff, DiffHunkStatus, DiffHunkStatusKind};

pub(crate) fn format_stack_review_comment_timestamp_at_offset(
    timestamp: &str,
    reference: ::time::OffsetDateTime,
    offset: ::time::UtcOffset,
) -> String {
    let Ok(timestamp) =
        ::time::OffsetDateTime::parse(timestamp, &::time::format_description::well_known::Rfc3339)
    else {
        return String::new();
    };
    time_format::format_localized_timestamp(
        timestamp,
        reference,
        offset,
        time_format::TimestampFormat::EnhancedAbsolute,
    )
}

pub fn format_stack_review_comment_timestamp(timestamp: &str) -> String {
    format_stack_review_comment_timestamp_at_offset(
        timestamp,
        ::time::OffsetDateTime::now_utc(),
        ::time::UtcOffset::current_local_offset().unwrap_or(::time::UtcOffset::UTC),
    )
}

#[derive(Clone)]
pub struct ResolvedDiffHunk {
    pub buffer_range: Range<text::Anchor>,
    pub diff_base_byte_range: Range<usize>,
    pub status: DiffHunkStatus,
}

#[derive(Clone)]
pub struct ResolvedDiffHunks {
    pub diff: Entity<BufferDiff>,
    pub buffer_id: BufferId,
    pub buffer: Option<Entity<Buffer>>,
    pub hunks: Vec<ResolvedDiffHunk>,
}

pub trait DiffHunkDelegate {
    fn toggle(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    );

    fn stage_or_unstage(
        &self,
        stage: bool,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    );

    fn restore(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        if hunks.is_empty() || editor.read_only(cx) {
            return;
        }
        self.stage_or_unstage(false, hunks.clone(), editor, window, cx);
        editor.transact(window, cx, |editor, window, cx| {
            editor.restore_diff_hunks(hunks, cx);
            let selections = editor
                .selections
                .all::<MultiBufferOffset>(&editor.display_snapshot(cx));
            editor.change_selections(
                SelectionEffects::no_scroll(),
                window,
                cx,
                |selections_state| {
                    selections_state.select(selections);
                },
            );
        });
    }

    fn render_hunk_controls(
        &self,
        row: u32,
        status: &DiffHunkStatus,
        hunk_range: Range<Anchor>,
        is_created_file: bool,
        line_height: Pixels,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement;

    fn render_hunk_as_staged(&self, status: &DiffHunkStatus, _cx: &App) -> bool {
        !status.has_secondary_hunk()
    }
}

pub struct UncommittedDiffHunkDelegate;

impl DiffHunkDelegate for UncommittedDiffHunkDelegate {
    fn toggle(
        &self,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let stage = hunks
            .iter()
            .flat_map(|hunks| hunks.hunks.iter())
            .any(|hunk| hunk.status.has_secondary_hunk());
        self.stage_or_unstage(stage, hunks, editor, window, cx);
    }

    fn stage_or_unstage(
        &self,
        stage: bool,
        hunks: Vec<ResolvedDiffHunks>,
        editor: &mut Editor,
        _window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let Some(project) = editor.project() else {
            return;
        };
        for hunks in hunks {
            let Some(buffer) = hunks.buffer else {
                continue;
            };

            let ranges = hunks
                .hunks
                .into_iter()
                .map(|hunk| hunk.buffer_range)
                .collect::<Vec<_>>();
            if ranges.is_empty() {
                continue;
            }
            let secondary_diff = hunks.diff.read(cx).secondary_diff();
            project
                .update(cx, |project, cx| {
                    if stage {
                        let Some(secondary_diff) = secondary_diff else {
                            return Err(anyhow::anyhow!("diff has no unstaged secondary"));
                        };
                        project.stage_hunks(buffer, secondary_diff, ranges, cx)
                    } else {
                        project.unstage_uncommitted_hunks(buffer, hunks.diff, ranges, cx)
                    }
                })
                .log_err();
        }
    }

    fn render_hunk_controls(
        &self,
        row: u32,
        status: &DiffHunkStatus,
        hunk_range: Range<Anchor>,
        is_created_file: bool,
        line_height: Pixels,
        editor: &Entity<Editor>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        render_diff_hunk_controls(
            row,
            status,
            hunk_range,
            is_created_file,
            line_height,
            editor,
            window,
            cx,
        )
    }
}

pub struct RestoreOnlyDiffHunkDelegate;

impl DiffHunkDelegate for RestoreOnlyDiffHunkDelegate {
    fn toggle(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn stage_or_unstage(
        &self,
        _stage: bool,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn restore(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn render_hunk_controls(
        &self,
        _row: u32,
        _status: &DiffHunkStatus,
        _hunk_range: Range<Anchor>,
        _is_created_file: bool,
        _line_height: Pixels,
        _editor: &Entity<Editor>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        gpui::Empty.into_any_element()
    }
}

pub struct RestoreOnlyUnstagedDiffHunkDelegate;

impl DiffHunkDelegate for RestoreOnlyUnstagedDiffHunkDelegate {
    fn toggle(
        &self,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn stage_or_unstage(
        &self,
        _stage: bool,
        _hunks: Vec<ResolvedDiffHunks>,
        _editor: &mut Editor,
        _window: &mut Window,
        _cx: &mut Context<Editor>,
    ) {
    }

    fn render_hunk_controls(
        &self,
        _row: u32,
        _status: &DiffHunkStatus,
        _hunk_range: Range<Anchor>,
        _is_created_file: bool,
        _line_height: Pixels,
        _editor: &Entity<Editor>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        gpui::Empty.into_any_element()
    }

    fn render_hunk_as_staged(&self, _status: &DiffHunkStatus, _cx: &App) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DisplayDiffHunk {
    Folded {
        display_row: DisplayRow,
    },
    Unfolded {
        is_created_file: bool,
        diff_base_byte_range: Range<usize>,
        display_row_range: Range<DisplayRow>,
        multi_buffer_range: Range<Anchor>,
        status: DiffHunkStatus,
        word_diffs: Vec<Range<MultiBufferOffset>>,
    },
}

#[derive(Clone)]
pub(super) struct InlineBlamePopoverState {
    pub(super) scroll_handle: ScrollHandle,
    pub(super) commit_message: Option<ParsedCommitMessage>,
    pub(super) markdown: Entity<Markdown>,
}

pub(super) struct InlineBlamePopover {
    pub(super) position: gpui::Point<Pixels>,
    pub(super) hide_task: Option<Task<()>>,
    pub(super) popover_bounds: Option<Bounds<Pixels>>,
    pub(super) popover_state: InlineBlamePopoverState,
    pub(super) keyboard_grace: bool,
}

/// Represents a diff review button indicator that shows up when hovering over lines in the gutter
/// in diff view mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhantomDiffReviewIndicator {
    /// The starting anchor of the selection (or the only row if not dragging).
    pub(super) start: Anchor,
    /// The ending anchor of the selection. Equal to start_anchor for single-line selection.
    pub(super) end: Anchor,
    /// There's a small debounce between hovering over the line and showing the indicator.
    /// We don't want to show the indicator when moving the mouse from editor to e.g. project panel.
    pub(super) is_active: bool,
}

#[derive(Clone, Debug)]
pub(super) struct DiffReviewDragState {
    start_anchor: Anchor,
    current_anchor: Anchor,
}

// Custom block height is fixed before text layout, so reserve against a narrow
// comment column rather than risking overlap when the editor is split.
const STACK_REVIEW_COMMENT_WRAP_COLUMNS: usize = 24;

/// Identifies a specific hunk in the diff buffer.
/// Used as a key to group comments by their location.
#[derive(Clone, Debug)]
pub(super) struct DiffHunkKey {
    /// The file path (relative to worktree) this hunk belongs to.
    pub(super) file_path: Arc<util::rel_path::RelPath>,
    /// An anchor at the start of the hunk. This tracks position as the buffer changes.
    pub(super) hunk_start_anchor: Anchor,
}

/// A review comment stored locally before being sent to the Agent panel.
#[derive(Clone)]
pub(super) struct StoredReviewComment {
    /// Unique identifier for this comment (for edit/delete operations).
    pub(super) id: usize,
    /// The comment text entered by the user.
    pub(super) comment: String,
    /// Anchors for the code range being reviewed.
    pub(super) range: Range<Anchor>,
    /// Whether this comment is currently being edited inline.
    pub(super) is_editing: bool,
    pub(super) author: StackReviewCommentAuthor,
    pub(super) source: StackReviewCommentSource,
    pub(super) reply_to: Option<usize>,
    pub(super) created_at: String,
    pub(super) created_at_display: SharedString,
    pub(super) resolved: bool,
}

/// Represents an active diff review overlay that appears when clicking the "Add Review" button.
pub(super) struct DiffReviewOverlay {
    pub(super) anchor_range: Range<Anchor>,
    /// The block ID for the overlay.
    pub(super) block_id: CustomBlockId,
    /// The editor entity for the review input.
    pub(super) prompt_editor: Entity<Editor>,
    /// The hunk key this overlay belongs to.
    pub(super) hunk_key: DiffHunkKey,
    /// Whether the comments section is expanded.
    pub(super) comments_expanded: bool,
    pub(super) composer_visible: bool,
    pub(super) comment_author: StackReviewCommentAuthor,
    pub(super) pending_reply_to: Option<usize>,
    /// Editors for comments currently being edited inline.
    /// Key: comment ID, Value: Editor entity for inline editing.
    pub(super) inline_edit_editors: HashMap<usize, Entity<Editor>>,
    /// Subscriptions for inline edit editors' action handlers.
    /// Key: comment ID, Value: Subscription keeping the Newline action handler alive.
    pub(super) inline_edit_subscriptions: HashMap<usize, Subscription>,
    /// The current user's avatar URI for display in comment rows.
    pub(super) user_avatar_uri: Option<SharedUri>,
    /// Subscription to keep the action handler alive.
    _subscription: Subscription,
}

impl DiffReviewDragState {
    pub(super) fn row_range(
        &self,
        snapshot: &DisplaySnapshot,
    ) -> std::ops::RangeInclusive<DisplayRow> {
        let start = self.start_anchor.to_display_point(snapshot).row();
        let current = self.current_anchor.to_display_point(snapshot).row();

        (start..=current).sorted()
    }
}

impl StoredReviewComment {
    fn with_metadata(
        id: usize,
        comment: String,
        anchor_range: Range<Anchor>,
        author: StackReviewCommentAuthor,
        source: StackReviewCommentSource,
        reply_to: Option<usize>,
    ) -> Self {
        let created_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let created_at_display = format_stack_review_comment_timestamp(&created_at).into();
        Self {
            id,
            comment,
            range: anchor_range,
            is_editing: false,
            author,
            source,
            reply_to,
            created_at,
            created_at_display,
            resolved: false,
        }
    }
}

#[derive(Clone)]
pub(super) enum StackReviewThreadItem {
    Comment {
        comment: StoredReviewComment,
        depth: usize,
    },
    Composer {
        depth: usize,
    },
}

pub(super) fn stack_review_thread_items(
    comments: Vec<StoredReviewComment>,
    pending_reply_to: Option<usize>,
) -> Vec<StackReviewThreadItem> {
    let mut comments_by_id = HashMap::default();
    for comment in comments {
        comments_by_id.entry(comment.id).or_insert(comment);
    }
    let known_ids = comments_by_id.keys().copied().collect::<HashSet<_>>();
    let mut roots = Vec::new();
    let mut children = HashMap::<usize, Vec<usize>>::default();
    for comment in comments_by_id.values() {
        if let Some(parent_id) = comment.reply_to
            && parent_id != comment.id
            && known_ids.contains(&parent_id)
        {
            children.entry(parent_id).or_default().push(comment.id);
        } else {
            roots.push(comment.id);
        }
    }
    roots.sort_unstable();
    for child_ids in children.values_mut() {
        child_ids.sort_unstable();
    }

    let mut items = Vec::new();
    let mut visited = HashSet::default();
    let mut pending_roots = roots;
    let mut all_ids = known_ids.into_iter().collect::<Vec<_>>();
    all_ids.sort_unstable();
    pending_roots.extend(all_ids);
    for root_id in pending_roots {
        if visited.contains(&root_id) {
            continue;
        }
        let mut pending = vec![(root_id, 0usize)];
        while let Some((comment_id, depth)) = pending.pop() {
            if !visited.insert(comment_id) {
                continue;
            }
            let Some(comment) = comments_by_id.get(&comment_id).cloned() else {
                continue;
            };
            items.push(StackReviewThreadItem::Comment { comment, depth });
            if pending_reply_to == Some(comment_id) {
                items.push(StackReviewThreadItem::Composer {
                    depth: depth.saturating_add(1),
                });
            }
            if let Some(child_ids) = children.get(&comment_id) {
                for child_id in child_ids.iter().rev() {
                    pending.push((*child_id, depth.saturating_add(1).min(32)));
                }
            }
        }
    }
    items
}

impl Editor {
    pub fn diff_hunks_in_ranges<'a>(
        &'a self,
        ranges: &'a [Range<Anchor>],
        buffer: &'a MultiBufferSnapshot,
    ) -> impl 'a + Iterator<Item = MultiBufferDiffHunk> {
        ranges.iter().flat_map(move |range| {
            let end_excerpt = buffer.excerpt_containing(range.end..range.end);
            let range = range.to_point(buffer);
            let mut peek_end = range.end;
            if range.end.row < buffer.max_row().0 {
                peek_end = Point::new(range.end.row + 1, 0);
            }
            buffer
                .diff_hunks_in_range(range.start..peek_end)
                .filter(move |hunk| {
                    if let Some((_, excerpt_range)) = &end_excerpt
                        && let Some(end_anchor) =
                            buffer.anchor_in_excerpt(excerpt_range.context.end)
                        && let Some(hunk_end_anchor) =
                            buffer.anchor_in_excerpt(hunk.excerpt_range.context.end)
                        && hunk_end_anchor.cmp(&end_anchor, buffer).is_gt()
                    {
                        false
                    } else {
                        true
                    }
                })
        })
    }

    fn resolve_diff_hunks(
        &self,
        hunks: Vec<MultiBufferDiffHunk>,
        cx: &App,
    ) -> Vec<ResolvedDiffHunks> {
        let multibuffer = self.buffer().read(cx);
        let chunk_by = hunks.into_iter().chunk_by(|hunk| hunk.buffer_id);
        let mut resolved = Vec::new();

        for (source_buffer_id, hunks) in &chunk_by {
            let Some(diff) = multibuffer.diff_for(source_buffer_id) else {
                continue;
            };
            let diff_snapshot = diff.read(cx).snapshot(cx);
            let main_buffer_id = diff_snapshot.buffer_id();
            let buffer = multibuffer.buffer(main_buffer_id).or_else(|| {
                self.project
                    .as_ref()
                    .and_then(|project| project.read(cx).buffer_for_id(main_buffer_id, cx))
            });
            let mut resolved_hunks = Vec::new();

            for hunk in hunks {
                if hunk.buffer_id == main_buffer_id {
                    resolved_hunks.push(ResolvedDiffHunk {
                        buffer_range: hunk.buffer_range,
                        diff_base_byte_range: hunk.diff_base_byte_range.start.0
                            ..hunk.diff_base_byte_range.end.0,
                        status: hunk.status,
                    });
                } else {
                    let diff_base_byte_range =
                        hunk.diff_base_byte_range.start.0..hunk.diff_base_byte_range.end.0;
                    let Some(hunk) = diff_snapshot
                        .hunks_intersecting_base_text_range(
                            diff_base_byte_range.clone(),
                            diff_snapshot.buffer_snapshot(),
                        )
                        .find(|hunk| hunk.diff_base_byte_range == diff_base_byte_range)
                    else {
                        continue;
                    };
                    let kind = if hunk.buffer_range.start == hunk.buffer_range.end {
                        DiffHunkStatusKind::Deleted
                    } else if hunk.diff_base_byte_range.is_empty() {
                        DiffHunkStatusKind::Added
                    } else {
                        DiffHunkStatusKind::Modified
                    };
                    resolved_hunks.push(ResolvedDiffHunk {
                        buffer_range: hunk.buffer_range,
                        diff_base_byte_range: hunk.diff_base_byte_range,
                        status: DiffHunkStatus {
                            kind,
                            secondary: hunk.secondary_status,
                        },
                    });
                }
            }

            if !resolved_hunks.is_empty() {
                resolved.push(ResolvedDiffHunks {
                    diff,
                    buffer_id: main_buffer_id,
                    buffer,
                    hunks: resolved_hunks,
                });
            }
        }

        resolved
    }

    pub fn diff_hunk_delegate(&self) -> Arc<dyn DiffHunkDelegate> {
        self.diff_hunk_delegate
            .clone()
            .unwrap_or_else(|| Arc::new(UncommittedDiffHunkDelegate))
    }

    pub fn set_diff_hunk_delegate(
        &mut self,
        delegate: Option<Arc<dyn DiffHunkDelegate>>,
        cx: &mut Context<Self>,
    ) {
        let had_delegate = self.diff_hunk_delegate.is_some();
        let has_delegate = delegate.is_some();
        self.diff_hunk_delegate = delegate;

        if !had_delegate && has_delegate {
            self.load_diff_task.take();
        } else if had_delegate && !has_delegate {
            self.buffer.update(cx, |buffer, cx| {
                buffer.set_all_diff_hunks_collapsed(cx);
            });

            if let Some(project) = self.project.clone() {
                self.load_diff_task = Some(
                    self.update_uncommitted_diff_for_buffer(
                        &project,
                        self.buffer.read(cx).all_buffers(),
                        cx,
                    )
                    .shared(),
                );
            }
        }

        cx.notify();
    }

    pub fn git_blame_inline_enabled(&self) -> bool {
        self.git_blame_inline_enabled
    }

    pub fn blame(&self) -> Option<&Entity<GitBlame>> {
        self.blame.as_ref()
    }

    pub fn active_git_blame_entry(&self, cx: &mut App) -> Option<BlameEntry> {
        if !self.show_git_blame_inline
            || self.newest_selection_head_on_empty_line(cx)
            || !self.has_blame_entries(cx)
        {
            return None;
        }

        let blame = self.blame.as_ref()?;
        let snapshot = self.display_snapshot(cx);
        let cursor = self.selections.newest::<Point>(&snapshot).head();
        let (buffer, point) = snapshot.buffer_snapshot().point_to_buffer_point(cursor)?;

        blame
            .update(cx, |blame, cx| {
                blame
                    .blame_for_rows(
                        &[RowInfo {
                            buffer_id: Some(buffer.remote_id()),
                            buffer_row: Some(point.row),
                            ..Default::default()
                        }],
                        cx,
                    )
                    .next()
            })
            .flatten()
            .map(|(_, entry)| entry)
    }

    pub fn show_git_blame_gutter(&self) -> bool {
        self.show_git_blame_gutter
    }

    pub fn expand_selected_diff_hunks(&mut self, cx: &mut Context<Self>) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        self.buffer
            .update(cx, |buffer, cx| buffer.expand_diff_hunks(ranges, cx))
    }

    pub fn toggle_git_blame(
        &mut self,
        _: &::git::Blame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_git_blame_gutter = !self.show_git_blame_gutter;

        if self.show_git_blame_gutter && !self.has_blame_entries(cx) {
            self.start_git_blame(true, window, cx);
        }

        cx.notify();
    }

    pub fn toggle_git_blame_inline(
        &mut self,
        _: &ToggleGitBlameInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_git_blame_inline_internal(true, window, cx);
        cx.notify();
    }

    /// Hides the inline blame popover element, in case it's already visible, or
    /// interrupts the task meant to show it, in case the task is running.
    ///
    /// When `ignore_timeout` is set to `true`, the popover is hidden
    /// immediately, otherwise it'll be hidden after a short delay.
    ///
    /// Returns `true` if the popover was visible and was hidden, `false`
    /// otherwise.
    pub fn hide_blame_popover(&mut self, ignore_timeout: bool, cx: &mut Context<Self>) -> bool {
        self.inline_blame_popover_show_task.take();

        if let Some(state) = &mut self.inline_blame_popover {
            if ignore_timeout {
                self.inline_blame_popover.take();
                cx.notify();
            } else {
                state.hide_task = Some(cx.spawn(async move |editor, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;

                    editor
                        .update(cx, |editor, cx| {
                            editor.inline_blame_popover.take();
                            cx.notify();
                        })
                        .ok();
                }));
            }

            true
        } else {
            false
        }
    }

    pub fn git_restore(&mut self, _: &Restore, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let selections = self
            .selections
            .all(&self.display_snapshot(cx))
            .into_iter()
            .map(|s| s.range())
            .collect();
        self.restore_hunks_in_ranges(selections, window, cx);
    }

    pub fn status_for_buffer_id(&self, buffer_id: BufferId, cx: &App) -> Option<FileStatus> {
        if let Some(status) = self
            .addons
            .iter()
            .find_map(|(_, addon)| addon.override_status_for_buffer_id(buffer_id, cx))
        {
            return Some(status);
        }
        self.project
            .as_ref()?
            .read(cx)
            .status_for_buffer_id(buffer_id, cx)
    }

    pub fn go_to_hunk_before_or_after_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        direction: Direction,
        wrap_around: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let row = if direction == Direction::Next {
            self.hunk_after_position(snapshot, position, wrap_around)
                .map(|hunk| hunk.row_range.start)
        } else {
            self.hunk_before_position(snapshot, position, wrap_around)
        };

        if let Some(row) = row {
            let destination = Point::new(row.0, 0);
            let autoscroll = Autoscroll::center();

            self.unfold_ranges(&[destination..destination], false, false, cx);
            self.change_selections(SelectionEffects::scroll(autoscroll), window, cx, |s| {
                s.select_ranges([destination..destination]);
            });
        }
    }

    pub fn set_expand_all_diff_hunks(&mut self, cx: &mut App) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_all_diff_hunks_expanded(cx);
        });
    }

    pub fn expand_all_diff_hunks(
        &mut self,
        _: &ExpandAllDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.expand_diff_hunks(vec![Anchor::Min..Anchor::Max], cx)
        });
    }

    pub fn review_file_path_at(
        &self,
        point: Point,
        cx: &App,
    ) -> Option<Arc<util::rel_path::RelPath>> {
        let multibuffer = self.buffer.read(cx);
        let snapshot = multibuffer.snapshot(cx);
        if let Some(file) = snapshot.file_at(point) {
            return Some(file.path().clone());
        }
        let buffer_id = snapshot
            .diff_hunks_in_range(Point::zero()..snapshot.max_point())
            .find(|hunk| {
                let start_row = hunk.row_range.start.0;
                let end_row = hunk.row_range.end.0;
                point.row >= start_row && point.row <= end_row
            })?
            .buffer_id;
        multibuffer
            .buffer(buffer_id)?
            .read(cx)
            .file()
            .map(|file| file.path().clone())
    }

    pub fn stack_review_comments(&self, cx: &App) -> Vec<StackReviewComment> {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut comments = self
            .stored_review_comments
            .iter()
            .flat_map(|(hunk_key, comments)| {
                comments.iter().filter_map(|comment| {
                    let start_point = comment.range.start.to_point(&snapshot);
                    let end_point = comment.range.end.to_point(&snapshot);
                    let (start_buffer, start) = snapshot.point_to_buffer_point(start_point)?;
                    let (end_buffer, end) = snapshot.point_to_buffer_point(end_point)?;
                    if start_buffer.remote_id() != end_buffer.remote_id() {
                        return None;
                    }
                    let path = if hunk_key.file_path.as_unix_str().is_empty() {
                        self.review_file_path_at(start_point, cx)?
                            .as_unix_str()
                            .to_owned()
                    } else {
                        hunk_key.file_path.as_unix_str().to_owned()
                    };
                    Some(StackReviewComment {
                        id: comment.id,
                        path,
                        start_row: start.row,
                        start_column: start.column,
                        end_row: end.row,
                        end_column: end.column,
                        body: comment.comment.clone(),
                        created_at: comment.created_at.clone(),
                        resolved: comment.resolved,
                        author: comment.author.clone(),
                        source: comment.source,
                        reply_to: comment.reply_to,
                    })
                })
            })
            .collect::<Vec<_>>();
        comments.sort_by_key(|comment| comment.id);
        comments
    }

    pub fn restore_stack_review_comments(
        &mut self,
        comments: &[StackReviewComment],
        cx: &mut Context<Self>,
    ) {
        let multibuffer = self.buffer.read(cx);
        let snapshot = multibuffer.snapshot(cx);
        let mut restored: Vec<(DiffHunkKey, Vec<StoredReviewComment>)> = Vec::new();
        let mut next_id = 0;

        for comment in comments {
            next_id = next_id.max(comment.id.saturating_add(1));
            let Some(file_path) = util::rel_path::RelPath::from_unix_str(&comment.path).ok() else {
                continue;
            };
            let Some(buffer) = multibuffer.all_buffers_iter().find(|buffer| {
                buffer
                    .read(cx)
                    .file()
                    .is_some_and(|file| file.path().as_ref() == file_path)
            }) else {
                continue;
            };
            let Some(start) = multibuffer.buffer_point_to_anchor(
                &buffer,
                Point::new(comment.start_row, comment.start_column),
                cx,
            ) else {
                continue;
            };
            let Some(end) = multibuffer.buffer_point_to_anchor(
                &buffer,
                Point::new(comment.end_row, comment.end_column),
                cx,
            ) else {
                continue;
            };
            let hunk_key = DiffHunkKey {
                file_path: Arc::from(file_path),
                hunk_start_anchor: start,
            };
            let stored_comment = StoredReviewComment {
                id: comment.id,
                comment: comment.body.clone(),
                range: start..end,
                is_editing: false,
                author: comment.author.clone(),
                source: comment.source,
                reply_to: comment.reply_to,
                created_at: comment.created_at.clone(),
                created_at_display: format_stack_review_comment_timestamp(&comment.created_at)
                    .into(),
                resolved: comment.resolved,
            };
            if let Some((_, existing_comments)) = restored.iter_mut().find(|(existing, _)| {
                existing.file_path == hunk_key.file_path
                    && existing.hunk_start_anchor.to_point(&snapshot)
                        == hunk_key.hunk_start_anchor.to_point(&snapshot)
            }) {
                existing_comments.push(stored_comment);
            } else {
                restored.push((hunk_key, vec![stored_comment]));
            }
        }

        self.stored_review_comments = restored;
        self.next_review_comment_id = next_id;
        cx.emit(EditorEvent::ReviewCommentsChanged {
            total_count: self.total_review_comment_count(),
        });
        cx.notify();
    }

    pub fn show_stack_review_comment_at_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.display_snapshot(cx);
        let row = self.selections.newest_display(&snapshot).head().row();
        self.show_diff_review_overlay(row..row, window, cx);
    }

    pub fn reveal_restored_stack_review_comments(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let comments_to_reveal = self
            .stored_review_comments
            .iter()
            .filter(|(_, comments)| !comments.is_empty())
            .map(|(hunk, _)| {
                (
                    hunk.clone(),
                    DisplayRow(hunk.hunk_start_anchor.to_point(&snapshot).row),
                )
            })
            .collect::<Vec<_>>();
        for (hunk_key, row) in comments_to_reveal {
            self.show_diff_review_overlay_internal(row..row, false, Some(hunk_key), window, cx);
        }
    }

    pub fn show_diff_review_overlay(
        &mut self,
        display_range: Range<DisplayRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_diff_review_overlay_internal(display_range, true, None, window, cx);
    }

    fn show_diff_review_overlay_internal(
        &mut self,
        display_range: Range<DisplayRow>,
        composer_visible: bool,
        restored_hunk_key: Option<DiffHunkKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Range { start, end } = display_range.sorted();

        let buffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let editor_snapshot = self.snapshot(window, cx);

        // Convert display rows to multibuffer points
        let start_point = editor_snapshot
            .display_snapshot
            .display_point_to_point(start.as_display_point(), Bias::Left);
        let end_point = editor_snapshot
            .display_snapshot
            .display_point_to_point(end.as_display_point(), Bias::Left);
        let end_multi_buffer_row = MultiBufferRow(end_point.row);

        // Create anchor range for the selected lines (start of first line to end of last line)
        let line_end = Point::new(
            end_point.row,
            buffer_snapshot.line_len(end_multi_buffer_row),
        );
        let anchor_range =
            buffer_snapshot.anchor_after(start_point)..buffer_snapshot.anchor_before(line_end);

        // Compute the hunk key for this display row
        let file_path = match self.review_file_path_at(start_point, cx) {
            Some(file_path) => file_path,
            None if self.is_stack_review => return,
            None => Arc::from(util::rel_path::RelPath::empty()),
        };
        if self.is_stack_review
            && self.review_file_path_at(end_point, cx).as_ref() != Some(&file_path)
        {
            return;
        }
        let hunk_start_anchor = buffer_snapshot.anchor_before(start_point);
        let new_hunk_key = if let Some(restored_hunk_key) = restored_hunk_key {
            if restored_hunk_key.file_path != file_path {
                return;
            }
            restored_hunk_key
        } else {
            DiffHunkKey {
                file_path,
                hunk_start_anchor,
            }
        };

        // Check if we already have an overlay for this hunk
        if let Some(overlay_index) = self.diff_review_overlays.iter().position(|overlay| {
            Self::hunk_keys_match(&overlay.hunk_key, &new_hunk_key, &buffer_snapshot)
        }) {
            let (prompt_editor, hunk_key, composer_was_hidden) = {
                let existing_overlay = &mut self.diff_review_overlays[overlay_index];
                let composer_was_hidden = composer_visible && !existing_overlay.composer_visible;
                existing_overlay.composer_visible |= composer_visible;
                if composer_visible {
                    existing_overlay.pending_reply_to = None;
                    existing_overlay
                        .prompt_editor
                        .update(cx, |prompt_editor, cx| {
                            prompt_editor.set_placeholder_text(
                                "Add a review comment...",
                                window,
                                cx,
                            );
                        });
                }
                (
                    existing_overlay.prompt_editor.clone(),
                    existing_overlay.hunk_key.clone(),
                    composer_was_hidden,
                )
            };
            if composer_was_hidden {
                self.refresh_diff_review_overlay_height(&hunk_key, window, cx);
            }
            if composer_visible {
                let focus_handle = prompt_editor.focus_handle(cx);
                window.focus(&focus_handle, cx);
            }
            return;
        }

        // Dismiss overlays that have no comments for their hunks
        self.dismiss_overlays_without_comments(cx);

        let (user_avatar_uri, comment_author) = self
            .project
            .as_ref()
            .and_then(|project| {
                let user_store = project.read(cx).user_store();
                let user = user_store.read(cx).current_user()?;
                Some((
                    Some(user.avatar_uri.clone()),
                    StackReviewCommentAuthor {
                        name: user
                            .name
                            .clone()
                            .unwrap_or_else(|| user.username.to_string()),
                        login: Some(user.username.to_string()),
                    },
                ))
            })
            .unwrap_or_else(|| (None, StackReviewCommentAuthor::default()));

        // Create anchor at the end of the last row so the block appears immediately below it
        // Use multibuffer coordinates for anchor creation
        let line_len = buffer_snapshot.line_len(end_multi_buffer_row);
        let anchor = buffer_snapshot.anchor_after(Point::new(end_multi_buffer_row.0, line_len));

        // Use the hunk key we already computed
        let hunk_key = new_hunk_key;

        // Create the prompt editor for the review input
        let prompt_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Add a review comment...", window, cx);
            editor
        });

        // Register the Newline action on the prompt editor to submit the review
        let parent_editor = cx.entity().downgrade();
        let subscription = prompt_editor.update(cx, |prompt_editor, _cx| {
            prompt_editor.register_action({
                let parent_editor = parent_editor.clone();
                move |_: &crate::actions::Newline, window, cx| {
                    if let Some(editor) = parent_editor.upgrade() {
                        editor.update(cx, |editor, cx| {
                            editor.submit_diff_review_comment(window, cx);
                        });
                    }
                }
            })
        });

        // Calculate initial height based on existing comments for this hunk
        let initial_height =
            self.calculate_overlay_height(&hunk_key, true, composer_visible, &buffer_snapshot);

        // Create the overlay block
        let prompt_editor_for_render = prompt_editor.clone();
        let hunk_key_for_render = hunk_key.clone();
        let editor_handle = cx.entity().downgrade();
        let block = BlockProperties {
            style: BlockStyle::Sticky,
            placement: BlockPlacement::Below(anchor),
            height: Some(initial_height),
            render: Arc::new(move |cx| {
                Self::render_diff_review_overlay(
                    &prompt_editor_for_render,
                    &hunk_key_for_render,
                    &editor_handle,
                    cx,
                )
            }),
            priority: 0,
        };

        let block_ids = self.insert_blocks([block], None, cx);
        let Some(block_id) = block_ids.into_iter().next() else {
            log::error!("Failed to insert diff review overlay block");
            return;
        };

        self.diff_review_overlays.push(DiffReviewOverlay {
            anchor_range,
            block_id,
            prompt_editor: prompt_editor.clone(),
            hunk_key,
            comments_expanded: true,
            composer_visible,
            comment_author,
            pending_reply_to: None,
            inline_edit_editors: HashMap::default(),
            inline_edit_subscriptions: HashMap::default(),
            user_avatar_uri,
            _subscription: subscription,
        });

        if composer_visible {
            let focus_handle = prompt_editor.focus_handle(cx);
            window.focus(&focus_handle, cx);
        }

        cx.notify();
    }

    /// Stores the diff review comment locally.
    /// Comments are stored per-hunk and can later be batch-submitted to the Agent panel.
    pub fn submit_diff_review_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Find the overlay that currently has focus
        let overlay_index = self
            .diff_review_overlays
            .iter()
            .position(|overlay| overlay.prompt_editor.focus_handle(cx).is_focused(window));
        let Some(overlay_index) = overlay_index else {
            return;
        };
        let overlay = &self.diff_review_overlays[overlay_index];

        let comment_text = overlay.prompt_editor.read(cx).text(cx).trim().to_string();
        if comment_text.is_empty() {
            return;
        }

        let anchor_range = overlay.anchor_range.clone();
        let hunk_key = overlay.hunk_key.clone();
        let author = overlay.comment_author.clone();
        let reply_to = overlay.pending_reply_to;

        self.add_review_comment_with_metadata(
            hunk_key.clone(),
            comment_text,
            anchor_range,
            author,
            StackReviewCommentSource::LocalHuman,
            reply_to,
            cx,
        );

        // Clear the prompt editor but keep the overlay open
        if let Some(overlay) = self.diff_review_overlays.get(overlay_index) {
            overlay.prompt_editor.update(cx, |editor, cx| {
                editor.clear(window, cx);
                editor.set_placeholder_text("Add a review comment...", window, cx);
            });
        }
        if self.is_stack_review
            && let Some(overlay) = self.diff_review_overlays.get_mut(overlay_index)
        {
            overlay.composer_visible = false;
            overlay.pending_reply_to = None;
        }

        // Refresh the overlay to update the block height for the new comment
        self.refresh_diff_review_overlay_height(&hunk_key, window, cx);

        cx.notify();
    }

    /// Returns the prompt editor for the diff review overlay, if one is active.
    /// This is primarily used for testing.
    pub fn diff_review_prompt_editor(&self) -> Option<&Entity<Editor>> {
        self.diff_review_overlays
            .first()
            .map(|overlay| &overlay.prompt_editor)
    }

    pub fn visible_stack_review_comment_count(&self, cx: &App) -> usize {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        self.diff_review_overlays
            .iter()
            .map(|overlay| self.hunk_comment_count(&overlay.hunk_key, &snapshot))
            .sum()
    }

    /// Sets whether the comments section is expanded in the diff review overlay.
    /// This is primarily used for testing.
    pub fn set_diff_review_comments_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        for overlay in &mut self.diff_review_overlays {
            overlay.comments_expanded = expanded;
        }
        cx.notify();
    }

    /// Returns the total count of stored review comments across all hunks.
    pub(super) fn total_review_comment_count(&self) -> usize {
        self.stored_review_comments
            .iter()
            .map(|(_, v)| v.len())
            .sum()
    }

    /// Adds a new review comment to a specific hunk.
    #[cfg(test)]
    pub(super) fn add_review_comment(
        &mut self,
        hunk_key: DiffHunkKey,
        comment: String,
        anchor_range: Range<Anchor>,
        cx: &mut Context<Self>,
    ) -> usize {
        self.add_review_comment_with_metadata(
            hunk_key,
            comment,
            anchor_range,
            StackReviewCommentAuthor::default(),
            StackReviewCommentSource::LocalHuman,
            None,
            cx,
        )
    }

    fn add_review_comment_with_metadata(
        &mut self,
        hunk_key: DiffHunkKey,
        comment: String,
        anchor_range: Range<Anchor>,
        author: StackReviewCommentAuthor,
        source: StackReviewCommentSource,
        reply_to: Option<usize>,
        cx: &mut Context<Self>,
    ) -> usize {
        let id = self.next_review_comment_id;
        self.next_review_comment_id += 1;

        let stored_comment =
            StoredReviewComment::with_metadata(id, comment, anchor_range, author, source, reply_to);

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let key_point = hunk_key.hunk_start_anchor.to_point(&snapshot);

        // Find existing entry for this hunk or add a new one
        if let Some((_, comments)) = self.stored_review_comments.iter_mut().find(|(k, _)| {
            k.file_path == hunk_key.file_path
                && k.hunk_start_anchor.to_point(&snapshot) == key_point
        }) {
            comments.push(stored_comment);
        } else {
            self.stored_review_comments
                .push((hunk_key, vec![stored_comment]));
        }

        cx.emit(EditorEvent::ReviewCommentsChanged {
            total_count: self.total_review_comment_count(),
        });
        cx.notify();
        id
    }

    pub(super) fn blame_hover(
        &mut self,
        _: &BlameHover,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let just_started = self.blame.is_none();
        if just_started {
            self.start_git_blame(true, window, cx);
        }
        let Some(blame) = self.blame.as_ref() else {
            return;
        };

        if just_started && !blame.read(cx).has_generated_entries() {
            let subscription = cx.observe_in(blame, window, |editor, blame, window, cx| {
                if blame.read(cx).has_generated_entries() {
                    editor.pending_blame_hover_observation.take();
                    editor.show_blame_hover_popover(window, cx);
                }
            });
            self.pending_blame_hover_observation = Some(subscription);
            return;
        }

        self.show_blame_hover_popover(window, cx);
    }

    fn show_blame_hover_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(window, cx);
        let cursor = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let Some((buffer, point)) = snapshot.buffer_snapshot().point_to_buffer_point(cursor) else {
            return;
        };

        let Some(blame) = self.blame.as_ref() else {
            return;
        };

        let row_info = RowInfo {
            buffer_id: Some(buffer.remote_id()),
            buffer_row: Some(point.row),
            ..Default::default()
        };
        let Some((buffer, blame_entry)) = blame
            .update(cx, |blame, cx| blame.blame_for_rows(&[row_info], cx).next())
            .flatten()
        else {
            return;
        };

        let anchor = self.selections.newest_anchor().head();
        let position = self.to_pixel_point(anchor, &snapshot, window, cx);
        if let (Some(position), Some(last_bounds)) = (position, self.last_bounds) {
            self.show_blame_popover(
                buffer,
                &blame_entry,
                position + last_bounds.origin,
                true,
                cx,
            );
        };
    }

    pub(super) fn restore_file(
        &mut self,
        _: &::git::RestoreFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let mut buffer_ids = HashSet::default();
        let snapshot = self.buffer().read(cx).snapshot(cx);
        for selection in self
            .selections
            .all::<MultiBufferOffset>(&self.display_snapshot(cx))
        {
            buffer_ids.extend(snapshot.buffer_ids_for_range(selection.range()))
        }

        let ranges = buffer_ids
            .into_iter()
            .flat_map(|buffer_id| snapshot.range_for_buffer(buffer_id))
            .collect::<Vec<_>>();

        self.restore_hunks_in_ranges(ranges, window, cx);
    }

    /// Restores the diff hunks in the editor's selections and moves the cursor
    /// to the next diff hunk. Wraps around to the beginning of the buffer if
    /// not all diff hunks are expanded.
    pub(super) fn restore_and_next(
        &mut self,
        _: &::git::RestoreAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let selections = self
            .selections
            .all(&self.display_snapshot(cx))
            .into_iter()
            .map(|selection| selection.range())
            .collect();

        self.restore_hunks_in_ranges(selections, window, cx);

        let all_diff_hunks_expanded = self.buffer().read(cx).all_diff_hunks_expanded();
        let wrap_around = !all_diff_hunks_expanded;
        let snapshot = self.snapshot(window, cx);
        let position = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();

        self.go_to_hunk_before_or_after_position(
            &snapshot,
            position,
            Direction::Next,
            wrap_around,
            window,
            cx,
        );
    }

    pub fn restore_diff_hunks(&mut self, hunks: Vec<ResolvedDiffHunks>, cx: &mut Context<Self>) {
        let mut revert_changes = Vec::new();
        for hunks in hunks {
            let Some(buffer) = hunks.buffer else {
                continue;
            };
            let diff_snapshot = hunks.diff.read(cx).snapshot(cx);
            let changes = hunks
                .hunks
                .into_iter()
                .filter_map(|hunk| {
                    if hunk.diff_base_byte_range == (0..0)
                        && hunk.buffer_range.start.is_min()
                        && hunk.buffer_range.end.is_max()
                    {
                        return None;
                    }
                    let original_text = diff_snapshot
                        .base_text()
                        .as_rope()
                        .slice(hunk.diff_base_byte_range.start..hunk.diff_base_byte_range.end);
                    Some((hunk.buffer_range, original_text))
                })
                .collect::<Vec<_>>();
            if !changes.is_empty() {
                revert_changes.push((buffer, changes));
            }
        }

        for (buffer, changes) in revert_changes {
            buffer.update(cx, |buffer, cx| {
                buffer.edit(
                    changes
                        .into_iter()
                        .map(|(range, text)| (range, text.to_string())),
                    None,
                    cx,
                );
            });
        }
    }

    pub(super) fn go_to_next_hunk(
        &mut self,
        _: &GoToHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
        self.go_to_hunk_before_or_after_position(
            &snapshot,
            selection.head(),
            Direction::Next,
            true,
            window,
            cx,
        );
    }

    pub(super) fn collapse_all_diff_hunks(
        &mut self,
        _: &CollapseAllDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.collapse_diff_hunks(vec![Anchor::Min..Anchor::Max], cx)
        });
    }

    pub fn toggle_all_diff_hunks(
        &mut self,
        _: &ToggleAllDiffHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_any_expanded_diff_hunks(cx) {
            self.collapse_all_diff_hunks(&CollapseAllDiffHunks, window, cx);
        } else {
            self.expand_all_diff_hunks(&ExpandAllDiffHunks, window, cx);
        }
    }

    pub(super) fn toggle_selected_diff_hunks(
        &mut self,
        _: &ToggleSelectedDiffHunks,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        self.toggle_diff_hunks_in_ranges(ranges, cx);
    }

    pub(super) fn show_diff_review_button(&self) -> bool {
        self.show_diff_review_button
    }

    pub(super) fn is_stack_review(&self) -> bool {
        self.is_stack_review
    }

    pub(super) fn render_diff_review_button(
        &self,
        display_row: DisplayRow,
        width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text_color = cx.theme().colors().text;
        let icon_color = cx.theme().colors().icon_accent;

        h_flex()
            .id("diff_review_button")
            .cursor_pointer()
            .w(width - px(1.))
            .h(relative(0.9))
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(text_color.opacity(0.1))
            .bg(text_color.opacity(0.15))
            .hover(|s| {
                s.bg(icon_color.opacity(0.4))
                    .border_color(icon_color.opacity(0.5))
            })
            .child(Icon::new(IconName::Plus).size(IconSize::Small))
            .tooltip(Tooltip::text("Add Review (drag to select multiple lines)"))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |editor, _event: &gpui::MouseDownEvent, window, cx| {
                    editor.start_diff_review_drag(display_row, window, cx);
                }),
            )
    }

    pub(super) fn start_diff_review_drag(
        &mut self,
        display_row: DisplayRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let point = snapshot
            .display_snapshot
            .display_point_to_point(DisplayPoint::new(display_row, 0), Bias::Left);
        let anchor = snapshot.buffer_snapshot().anchor_before(point);
        self.diff_review_drag_state = Some(DiffReviewDragState {
            start_anchor: anchor,
            current_anchor: anchor,
        });
        cx.notify();
    }

    pub(super) fn update_diff_review_drag(
        &mut self,
        display_row: DisplayRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.diff_review_drag_state.is_none() {
            return;
        }
        let snapshot = self.snapshot(window, cx);
        let point = snapshot
            .display_snapshot
            .display_point_to_point(display_row.as_display_point(), Bias::Left);
        let anchor = snapshot.buffer_snapshot().anchor_before(point);
        if let Some(drag_state) = &mut self.diff_review_drag_state {
            drag_state.current_anchor = anchor;
            cx.notify();
        }
    }

    pub(super) fn end_diff_review_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(drag_state) = self.diff_review_drag_state.take() {
            let snapshot = self.snapshot(window, cx);
            let range = drag_state.row_range(&snapshot.display_snapshot);
            self.show_diff_review_overlay(*range.start()..*range.end(), window, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_diff_review_drag(&mut self, cx: &mut Context<Self>) {
        self.diff_review_drag_state = None;
        cx.notify();
    }

    /// Dismisses all diff review overlays.
    pub(super) fn dismiss_all_diff_review_overlays(&mut self, cx: &mut Context<Self>) {
        if self.diff_review_overlays.is_empty() {
            return;
        }
        let block_ids: HashSet<_> = self
            .diff_review_overlays
            .drain(..)
            .map(|overlay| overlay.block_id)
            .collect();
        self.remove_blocks(block_ids, None, cx);
        cx.notify();
    }

    /// Action handler for SubmitDiffReviewComment.
    pub(super) fn submit_diff_review_comment_action(
        &mut self,
        _: &SubmitDiffReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit_diff_review_comment(window, cx);
    }

    /// Returns comments for a specific hunk, ordered by creation time.
    pub(super) fn comments_for_hunk<'a>(
        &'a self,
        key: &DiffHunkKey,
        snapshot: &MultiBufferSnapshot,
    ) -> &'a [StoredReviewComment] {
        let key_point = key.hunk_start_anchor.to_point(snapshot);
        self.stored_review_comments
            .iter()
            .find(|(k, _)| {
                k.file_path == key.file_path && k.hunk_start_anchor.to_point(snapshot) == key_point
            })
            .map(|(_, comments)| comments.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the count of comments for a specific hunk.
    pub(super) fn hunk_comment_count(
        &self,
        key: &DiffHunkKey,
        snapshot: &MultiBufferSnapshot,
    ) -> usize {
        let key_point = key.hunk_start_anchor.to_point(snapshot);
        self.stored_review_comments
            .iter()
            .find(|(k, _)| {
                k.file_path == key.file_path && k.hunk_start_anchor.to_point(snapshot) == key_point
            })
            .map(|(_, v)| v.len())
            .unwrap_or(0)
    }

    /// Removes a review comment by ID from any hunk.
    pub(super) fn remove_review_comment(&mut self, id: usize, cx: &mut Context<Self>) -> bool {
        let mut removed = false;
        for (_, comments) in &mut self.stored_review_comments {
            if let Some(index) = comments.iter().position(|comment| comment.id == id) {
                comments.remove(index);
                removed = true;
                break;
            }
        }
        if !removed {
            return false;
        }
        for (_, comments) in &mut self.stored_review_comments {
            for comment in comments {
                if comment.reply_to == Some(id) {
                    comment.reply_to = None;
                }
            }
        }
        cx.emit(EditorEvent::ReviewCommentsChanged {
            total_count: self.total_review_comment_count(),
        });
        cx.notify();
        true
    }

    /// Updates a review comment's text by ID.
    pub(super) fn update_review_comment(
        &mut self,
        id: usize,
        new_comment: String,
        cx: &mut Context<Self>,
    ) -> bool {
        for (_, comments) in self.stored_review_comments.iter_mut() {
            if let Some(comment) = comments.iter_mut().find(|c| c.id == id) {
                comment.comment = new_comment;
                comment.is_editing = false;
                cx.emit(EditorEvent::ReviewCommentsChanged {
                    total_count: self.total_review_comment_count(),
                });
                cx.notify();
                return true;
            }
        }
        false
    }

    pub fn set_review_comment_resolved(
        &mut self,
        id: usize,
        resolved: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut root_id = id;
        let mut seen = HashSet::default();
        loop {
            if !seen.insert(root_id) {
                return false;
            }
            let parent = self
                .stored_review_comments
                .iter()
                .flat_map(|(_, comments)| comments)
                .find(|comment| comment.id == root_id)
                .and_then(|comment| comment.reply_to);
            let Some(parent) = parent else {
                break;
            };
            root_id = parent;
        }
        let mut ids = vec![root_id];
        loop {
            let mut changed = false;
            for comment in self
                .stored_review_comments
                .iter()
                .flat_map(|(_, comments)| comments)
            {
                if comment.reply_to.is_some_and(|parent| ids.contains(&parent))
                    && !ids.contains(&comment.id)
                {
                    ids.push(comment.id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut found = false;
        for (_, comments) in &mut self.stored_review_comments {
            for comment in comments {
                if ids.contains(&comment.id) {
                    comment.resolved = resolved;
                    found = true;
                }
            }
        }
        if found {
            cx.emit(EditorEvent::ReviewCommentResolutionChanged { ids, resolved });
            cx.notify();
        }
        found
    }

    pub(super) fn toggle_active_review_comment_resolved(
        &mut self,
        _: &crate::actions::ToggleActiveReviewCommentResolved,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_stack_review {
            cx.propagate();
            return;
        }
        let display_snapshot = self.display_snapshot(cx);
        let cursor = self.selections.newest::<Point>(&display_snapshot).head();
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let candidate = self
            .stored_review_comments
            .iter()
            .flat_map(|(_, comments)| comments)
            .filter(|comment| {
                let start = comment.range.start.to_point(&snapshot);
                let end = comment.range.end.to_point(&snapshot);
                start.row <= cursor.row && cursor.row <= end.row
            })
            .min_by_key(|comment| comment.resolved)
            .map(|comment| (comment.id, comment.resolved));
        if let Some((id, resolved)) = candidate {
            self.set_review_comment_resolved(id, !resolved, cx);
        }
    }

    /// Sets a comment's editing state.
    pub(super) fn set_comment_editing(
        &mut self,
        id: usize,
        is_editing: bool,
        cx: &mut Context<Self>,
    ) {
        for (_, comments) in self.stored_review_comments.iter_mut() {
            if let Some(comment) = comments.iter_mut().find(|c| c.id == id) {
                comment.is_editing = is_editing;
                cx.notify();
                return;
            }
        }
    }

    /// Removes review comments whose anchors are no longer valid or whose
    /// associated diff hunks no longer exist.
    ///
    /// This should be called when the buffer changes to prevent orphaned comments
    /// from accumulating.
    pub(super) fn cleanup_orphaned_review_comments(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let original_count = self.total_review_comment_count();

        // Remove comments with invalid hunk anchors
        self.stored_review_comments
            .retain(|(hunk_key, _)| hunk_key.hunk_start_anchor.is_valid(&snapshot));

        // Also clean up individual comments with invalid anchor ranges
        for (_, comments) in &mut self.stored_review_comments {
            comments.retain(|comment| {
                comment.range.start.is_valid(&snapshot) && comment.range.end.is_valid(&snapshot)
            });
        }

        // Remove empty hunk entries
        self.stored_review_comments
            .retain(|(_, comments)| !comments.is_empty());

        let new_count = self.total_review_comment_count();
        if new_count != original_count {
            cx.emit(EditorEvent::ReviewCommentsChanged {
                total_count: new_count,
            });
            cx.notify();
        }
    }

    /// Toggles the expanded state of the comments section in the overlay.
    pub(super) fn toggle_review_comments_expanded(
        &mut self,
        _: &ToggleReviewCommentsExpanded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Find the overlay that currently has focus, or use the first one
        let overlay_info = self.diff_review_overlays.iter_mut().find_map(|overlay| {
            if overlay.prompt_editor.focus_handle(cx).is_focused(window) {
                overlay.comments_expanded = !overlay.comments_expanded;
                Some(overlay.hunk_key.clone())
            } else {
                None
            }
        });

        // If no focused overlay found, toggle the first one
        let hunk_key = overlay_info.or_else(|| {
            self.diff_review_overlays.first_mut().map(|overlay| {
                overlay.comments_expanded = !overlay.comments_expanded;
                overlay.hunk_key.clone()
            })
        });

        if let Some(hunk_key) = hunk_key {
            self.refresh_diff_review_overlay_height(&hunk_key, window, cx);
            cx.notify();
        }
    }

    fn toggle_review_comments_for_hunk(
        &mut self,
        hunk_key: &DiffHunkKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let Some(overlay) = self
            .diff_review_overlays
            .iter_mut()
            .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, hunk_key, &snapshot))
        else {
            return;
        };
        overlay.comments_expanded = !overlay.comments_expanded;
        let hunk_key = overlay.hunk_key.clone();
        self.refresh_diff_review_overlay_height(&hunk_key, window, cx);
        cx.notify();
    }

    pub(super) fn reply_to_review_comment(
        &mut self,
        action: &ReplyToReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let comment_id = action.id;
        let Some(hunk_key) = self
            .stored_review_comments
            .iter()
            .find_map(|(key, comments)| {
                comments
                    .iter()
                    .any(|comment| comment.id == comment_id)
                    .then(|| key.clone())
            })
        else {
            return;
        };
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let Some(overlay) = self
            .diff_review_overlays
            .iter_mut()
            .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, &hunk_key, &snapshot))
        else {
            return;
        };
        overlay.pending_reply_to = Some(comment_id);
        overlay.composer_visible = true;
        overlay.prompt_editor.update(cx, |prompt_editor, cx| {
            prompt_editor.clear(window, cx);
            prompt_editor.set_placeholder_text("Write a reply...", window, cx);
        });
        let prompt_editor = overlay.prompt_editor.clone();
        self.refresh_diff_review_overlay_height(&hunk_key, window, cx);
        window.focus(&prompt_editor.focus_handle(cx), cx);
        cx.notify();
    }

    /// Handles the EditReviewComment action - sets a comment into editing mode.
    pub(super) fn edit_review_comment(
        &mut self,
        action: &EditReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let comment_id = action.id;
        if self
            .stored_review_comments
            .iter()
            .flat_map(|(_, comments)| comments)
            .find(|comment| comment.id == comment_id)
            .is_some_and(|comment| comment.source == StackReviewCommentSource::Github)
        {
            return;
        }

        // Set the comment to editing mode
        self.set_comment_editing(comment_id, true, cx);

        // Find the overlay that contains this comment and create an inline editor if needed
        // First, find which hunk this comment belongs to
        let hunk_key = self
            .stored_review_comments
            .iter()
            .find_map(|(key, comments)| {
                if comments.iter().any(|c| c.id == comment_id) {
                    Some(key.clone())
                } else {
                    None
                }
            });

        let snapshot = self.buffer.read(cx).snapshot(cx);
        if let Some(hunk_key) = hunk_key {
            if let Some(overlay) = self
                .diff_review_overlays
                .iter_mut()
                .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, &hunk_key, &snapshot))
            {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    overlay.inline_edit_editors.entry(comment_id)
                {
                    // Find the comment text
                    let comment_text = self
                        .stored_review_comments
                        .iter()
                        .flat_map(|(_, comments)| comments)
                        .find(|c| c.id == comment_id)
                        .map(|c| c.comment.clone())
                        .unwrap_or_default();

                    // Create inline editor
                    let parent_editor = cx.entity().downgrade();
                    let inline_editor = cx.new(|cx| {
                        let mut editor = Editor::single_line(window, cx);
                        editor.set_text(&*comment_text, window, cx);
                        // Select all text for easy replacement
                        editor.select_all(&crate::actions::SelectAll, window, cx);
                        editor
                    });

                    // Register the Newline action to confirm the edit
                    let subscription = inline_editor.update(cx, |inline_editor, _cx| {
                        inline_editor.register_action({
                            let parent_editor = parent_editor.clone();
                            move |_: &crate::actions::Newline, window, cx| {
                                if let Some(editor) = parent_editor.upgrade() {
                                    editor.update(cx, |editor, cx| {
                                        editor.confirm_edit_review_comment(comment_id, window, cx);
                                    });
                                }
                            }
                        })
                    });

                    // Store the subscription to keep the action handler alive
                    overlay
                        .inline_edit_subscriptions
                        .insert(comment_id, subscription);

                    // Focus the inline editor
                    let focus_handle = inline_editor.focus_handle(cx);
                    window.focus(&focus_handle, cx);

                    entry.insert(inline_editor);
                }
            }
        }

        cx.notify();
    }

    /// Confirms an inline edit of a review comment.
    pub(super) fn confirm_edit_review_comment(
        &mut self,
        comment_id: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Get the new text from the inline editor
        // Find the overlay containing this comment's inline editor
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let hunk_key = self
            .stored_review_comments
            .iter()
            .find_map(|(key, comments)| {
                if comments.iter().any(|c| c.id == comment_id) {
                    Some(key.clone())
                } else {
                    None
                }
            });

        let new_text = hunk_key
            .as_ref()
            .and_then(|hunk_key| {
                self.diff_review_overlays
                    .iter()
                    .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, hunk_key, &snapshot))
            })
            .as_ref()
            .and_then(|overlay| overlay.inline_edit_editors.get(&comment_id))
            .map(|editor| editor.read(cx).text(cx).trim().to_string());

        if let Some(new_text) = new_text {
            if !new_text.is_empty() {
                self.update_review_comment(comment_id, new_text, cx);
            }
        }

        // Remove the inline editor and its subscription
        if let Some(hunk_key) = hunk_key {
            if let Some(overlay) = self
                .diff_review_overlays
                .iter_mut()
                .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, &hunk_key, &snapshot))
            {
                overlay.inline_edit_editors.remove(&comment_id);
                overlay.inline_edit_subscriptions.remove(&comment_id);
            }
        }

        // Clear editing state
        self.set_comment_editing(comment_id, false, cx);
    }

    /// Cancels an inline edit of a review comment.
    pub(super) fn cancel_edit_review_comment(
        &mut self,
        comment_id: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Find which hunk this comment belongs to
        let hunk_key = self
            .stored_review_comments
            .iter()
            .find_map(|(key, comments)| {
                if comments.iter().any(|c| c.id == comment_id) {
                    Some(key.clone())
                } else {
                    None
                }
            });

        // Remove the inline editor and its subscription
        if let Some(hunk_key) = hunk_key {
            let snapshot = self.buffer.read(cx).snapshot(cx);
            if let Some(overlay) = self
                .diff_review_overlays
                .iter_mut()
                .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, &hunk_key, &snapshot))
            {
                overlay.inline_edit_editors.remove(&comment_id);
                overlay.inline_edit_subscriptions.remove(&comment_id);
            }
        }

        // Clear editing state
        self.set_comment_editing(comment_id, false, cx);
    }

    /// Action handler for ConfirmEditReviewComment.
    pub(super) fn confirm_edit_review_comment_action(
        &mut self,
        action: &ConfirmEditReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_edit_review_comment(action.id, window, cx);
    }

    /// Action handler for CancelEditReviewComment.
    pub(super) fn cancel_edit_review_comment_action(
        &mut self,
        action: &CancelEditReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_edit_review_comment(action.id, window, cx);
    }

    /// Handles the DeleteReviewComment action - removes a comment.
    pub(super) fn delete_review_comment(
        &mut self,
        action: &DeleteReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Get the hunk key before removing the comment
        // Find the hunk key from the comment itself
        let comment_id = action.id;
        if self
            .stored_review_comments
            .iter()
            .flat_map(|(_, comments)| comments)
            .find(|comment| comment.id == comment_id)
            .is_some_and(|comment| comment.source == StackReviewCommentSource::Github)
        {
            return;
        }
        let hunk_key = self
            .stored_review_comments
            .iter()
            .find_map(|(key, comments)| {
                if comments.iter().any(|c| c.id == comment_id) {
                    Some(key.clone())
                } else {
                    None
                }
            });

        // Also get it from the overlay for refresh purposes
        let overlay_hunk_key = self
            .diff_review_overlays
            .first()
            .map(|o| o.hunk_key.clone());

        self.remove_review_comment(action.id, cx);

        if let Some(hunk_key) = hunk_key.or(overlay_hunk_key) {
            let snapshot = self.buffer.read(cx).snapshot(cx);
            if self.hunk_comment_count(&hunk_key, &snapshot) == 0 {
                if let Some(index) = self.diff_review_overlays.iter().position(|overlay| {
                    Self::hunk_keys_match(&overlay.hunk_key, &hunk_key, &snapshot)
                }) {
                    let overlay = self.diff_review_overlays.remove(index);
                    let mut block_ids = HashSet::default();
                    block_ids.insert(overlay.block_id);
                    self.remove_blocks(block_ids, None, cx);
                }
            } else {
                self.refresh_diff_review_overlay_height(&hunk_key, window, cx);
            }
        }
    }

    pub(super) fn copy_permalink_to_line(
        &mut self,
        _: &CopyPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, async move |_, cx| match permalink_task.await {
            Ok(permalink) => {
                cx.update(|_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(permalink.to_string()));
                })
                .ok();
            }
            Err(err) => {
                let message = format!("Failed to copy permalink: {err}");

                anyhow::Result::<()>::Err(err).log_err();

                if let Some(workspace) = workspace {
                    workspace
                        .update_in(cx, |workspace, _, cx| {
                            struct CopyPermalinkToLine;

                            workspace.show_toast(
                                Toast::new(
                                    NotificationId::unique::<CopyPermalinkToLine>(),
                                    message,
                                ),
                                cx,
                            )
                        })
                        .ok();
                }
            }
        })
        .detach();
    }

    pub(super) fn open_permalink_to_line(
        &mut self,
        _: &OpenPermalinkToLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let permalink_task = self.get_permalink_to_line(cx);
        let workspace = self.workspace();

        cx.spawn_in(window, async move |_, cx| match permalink_task.await {
            Ok(permalink) => {
                cx.update(|_, cx| {
                    cx.open_url(permalink.as_ref());
                })
                .ok();
            }
            Err(err) => {
                let message = format!("Failed to open permalink: {err}");

                anyhow::Result::<()>::Err(err).log_err();

                if let Some(workspace) = workspace {
                    workspace.update(cx, |workspace, cx| {
                        struct OpenPermalinkToLine;

                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<OpenPermalinkToLine>(), message),
                            cx,
                        )
                    });
                }
            }
        })
        .detach();
    }

    pub(super) fn toggle_staged_selected_diff_hunks(
        &mut self,
        _: &::git::ToggleStaged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges: Vec<_> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|s| s.range())
            .collect();
        let task = self.save_buffers_for_ranges_if_needed(&ranges, cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await?;
            this.update_in(cx, |this, window, cx| {
                let snapshot = this.buffer.read(cx).snapshot(cx);
                let hunks = this.diff_hunks_in_ranges(&ranges, &snapshot).collect();
                this.apply_toggle(hunks, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    pub(super) fn stage_and_next(
        &mut self,
        _: &::git::StageAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_stage_or_unstage_and_next(true, window, cx);
    }

    pub(super) fn unstage_and_next(
        &mut self,
        _: &::git::UnstageAndNext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.do_stage_or_unstage_and_next(false, window, cx);
    }

    pub fn apply_toggle(
        &mut self,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.toggle(hunks, self, window, cx);
    }

    pub fn apply_stage_or_unstage(
        &mut self,
        stage: bool,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.stage_or_unstage(stage, hunks, self, window, cx);
    }

    pub fn apply_restore(
        &mut self,
        hunks: Vec<MultiBufferDiffHunk>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut hunks = self.resolve_diff_hunks(hunks, cx);
        if self.diff_hunk_delegate.is_none() {
            hunks.retain(|hunks| hunks.diff.read(cx).is_stageable());
        }
        if hunks.is_empty() {
            return;
        }
        let delegate = self.diff_hunk_delegate();
        delegate.restore(hunks, self, window, cx);
    }

    pub(super) fn clear_expanded_diff_hunks(&mut self, cx: &mut Context<Self>) -> bool {
        self.buffer.update(cx, |buffer, cx| {
            let ranges = vec![Anchor::Min..Anchor::Max];
            if !buffer.all_diff_hunks_expanded()
                && buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx)
            {
                buffer.collapse_diff_hunks(ranges, cx);
                true
            } else {
                false
            }
        })
    }

    pub(super) fn has_any_expanded_diff_hunks(&self, cx: &App) -> bool {
        if self.buffer.read(cx).all_diff_hunks_expanded() {
            return true;
        }
        let ranges = vec![Anchor::Min..Anchor::Max];
        self.buffer
            .read(cx)
            .has_expanded_diff_hunks_in_ranges(&ranges, cx)
    }

    pub(super) fn toggle_single_diff_hunk(&mut self, range: Range<Anchor>, cx: &mut Context<Self>) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.toggle_single_diff_hunk(range, cx);
        })
    }

    pub(super) fn apply_all_diff_hunks(
        &mut self,
        _: &ApplyAllDiffHunks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let buffers = self.buffer.read(cx).all_buffers();
        for branch_buffer in buffers {
            branch_buffer.update(cx, |branch_buffer, cx| {
                branch_buffer.merge_into_base(Vec::new(), cx);
            });
        }

        if let Some(project) = self.project.clone() {
            self.save(
                SaveOptions {
                    format: true,
                    force_format: false,
                    autosave: false,
                },
                project,
                window,
                cx,
            )
            .detach_and_log_err(cx);
        }
    }

    pub(super) fn apply_selected_diff_hunks(
        &mut self,
        _: &ApplyDiffHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let snapshot = self.snapshot(window, cx);
        let hunks = snapshot.hunks_for_ranges(
            self.selections
                .all(&snapshot.display_snapshot)
                .into_iter()
                .map(|selection| selection.range()),
        );
        let mut ranges_by_buffer = HashMap::default();
        self.transact(window, cx, |editor, _window, cx| {
            for hunk in hunks {
                if let Some(buffer) = editor.buffer.read(cx).buffer(hunk.buffer_id) {
                    ranges_by_buffer
                        .entry(buffer.clone())
                        .or_insert_with(Vec::new)
                        .push(hunk.buffer_range.to_offset(buffer.read(cx)));
                }
            }

            for (buffer, ranges) in ranges_by_buffer {
                buffer.update(cx, |buffer, cx| {
                    buffer.merge_into_base(ranges, cx);
                });
            }
        });

        if let Some(project) = self.project.clone() {
            self.save(
                SaveOptions {
                    format: true,
                    force_format: false,
                    autosave: false,
                },
                project,
                window,
                cx,
            )
            .detach_and_log_err(cx);
        }
    }

    pub(super) fn open_git_blame_commit(
        &mut self,
        _: &OpenGitBlameCommit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_git_blame_commit_internal(window, cx);
    }

    pub(super) fn toggle_git_blame_inline_internal(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.git_blame_inline_enabled {
            self.git_blame_inline_enabled = false;
            self.show_git_blame_inline = false;
            self.show_git_blame_inline_delay_task.take();
        } else {
            self.git_blame_inline_enabled = true;
            self.start_git_blame_inline(user_triggered, window, cx);
        }

        cx.notify();
    }

    pub(super) fn start_git_blame_inline(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.start_git_blame(user_triggered, window, cx);

        if ProjectSettings::get_global(cx)
            .git
            .inline_blame_delay()
            .is_some()
        {
            self.start_inline_blame_timer(window, cx);
        } else {
            self.show_git_blame_inline = true
        }
    }

    pub(super) fn render_git_blame_gutter(&self, cx: &App) -> bool {
        !self.mode().is_minimap() && self.show_git_blame_gutter && self.has_blame_entries(cx)
    }

    pub(super) fn render_git_blame_inline(&self, window: &Window, cx: &App) -> bool {
        ProjectSettings::get_global(cx).git.inline_blame.location
            == project::project_settings::InlineBlameLocation::Inline
            && self.show_git_blame_inline
            && (self.focus_handle.is_focused(window) || self.inline_blame_popover.is_some())
            && !self.newest_selection_head_on_empty_line(cx)
            && self.has_blame_entries(cx)
    }

    pub(super) fn start_inline_blame_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(delay) = ProjectSettings::get_global(cx).git.inline_blame_delay() {
            self.show_git_blame_inline = false;

            self.show_git_blame_inline_delay_task =
                Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;

                    this.update(cx, |this, cx| {
                        this.show_git_blame_inline = true;
                        cx.notify();
                    })
                    .log_err();
                }));
        }
    }

    pub(super) fn show_blame_popover(
        &mut self,
        buffer: BufferId,
        blame_entry: &BlameEntry,
        position: gpui::Point<Pixels>,
        ignore_timeout: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &mut self.inline_blame_popover {
            state.hide_task.take();
        } else {
            let blame_popover_delay = EditorSettings::get_global(cx).hover_popover_delay.0;
            let blame_entry = blame_entry.clone();
            let show_task = cx.spawn(async move |editor, cx| {
                if !ignore_timeout {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(blame_popover_delay))
                        .await;
                }
                editor
                    .update(cx, |editor, cx| {
                        editor.inline_blame_popover_show_task.take();
                        let Some(blame) = editor.blame.as_ref() else {
                            return;
                        };
                        let blame = blame.read(cx);
                        let details = blame.details_for_entry(buffer, &blame_entry);
                        let markdown = cx.new(|cx| {
                            Markdown::new(
                                details
                                    .as_ref()
                                    .map(|message| message.message.clone())
                                    .unwrap_or_default(),
                                None,
                                None,
                                cx,
                            )
                        });
                        editor.inline_blame_popover = Some(InlineBlamePopover {
                            position,
                            hide_task: None,
                            popover_bounds: None,
                            popover_state: InlineBlamePopoverState {
                                scroll_handle: ScrollHandle::new(),
                                commit_message: details,
                                markdown,
                            },
                            keyboard_grace: ignore_timeout,
                        });
                        cx.notify();
                    })
                    .ok();
            });
            self.inline_blame_popover_show_task = Some(show_task);
        }
    }

    pub(super) fn go_to_prev_hunk(
        &mut self,
        _: &GoToPreviousHunk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let selection = self.selections.newest::<Point>(&snapshot.display_snapshot);
        self.go_to_hunk_before_or_after_position(
            &snapshot,
            selection.head(),
            Direction::Prev,
            true,
            window,
            cx,
        );
    }

    /// Calculates the appropriate block height for the diff review overlay.
    /// Height is in lines: 2 for input row, 1 for header when comments exist,
    /// and 2 lines per comment when expanded.
    pub(super) fn calculate_overlay_height(
        &self,
        hunk_key: &DiffHunkKey,
        comments_expanded: bool,
        composer_visible: bool,
        snapshot: &MultiBufferSnapshot,
    ) -> u32 {
        let comments = self.comments_for_hunk(hunk_key, snapshot);
        let comment_count = comments.len();
        let base_height = if composer_visible { 2 } else { 0 };

        if comment_count == 0 {
            base_height
        } else if comments_expanded {
            let comments_height = comments.iter().fold(0u32, |height, comment| {
                let body_lines = comment.comment.split('\n').fold(0u32, |lines, line| {
                    let character_count = line.chars().count().max(1);
                    let visual_lines = character_count.div_ceil(STACK_REVIEW_COMMENT_WRAP_COLUMNS);
                    lines.saturating_add(u32::try_from(visual_lines).unwrap_or(u32::MAX))
                });
                height.saturating_add(body_lines.saturating_add(1))
            });
            base_height
                .saturating_add(1)
                .saturating_add(comments_height)
        } else {
            base_height + 1
        }
    }

    pub fn stage_or_unstage_diff_hunks(
        &mut self,
        stage: bool,
        ranges: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.save_buffers_for_ranges_if_needed(&ranges, cx);
        cx.spawn_in(window, async move |this, cx| {
            task.await?;
            this.update_in(cx, |this, window, cx| {
                let snapshot = this.buffer.read(cx).snapshot(cx);
                let hunks = this.diff_hunks_in_ranges(&ranges, &snapshot).collect();
                this.apply_stage_or_unstage(stage, hunks, window, cx);
            })
        })
        .detach_and_log_err(cx);
    }

    pub fn restore_diff_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let hunks = self.diff_hunks_in_ranges(&ranges, &snapshot).collect();
        self.apply_restore(hunks, window, cx);
    }

    fn toggle_diff_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Anchor>>,
        cx: &mut Context<Editor>,
    ) {
        self.buffer.update(cx, |buffer, cx| {
            let expand = !buffer.has_expanded_diff_hunks_in_ranges(&ranges, cx);
            buffer.expand_or_collapse_diff_hunks(ranges, expand, cx);
        })
    }

    fn start_git_blame(
        &mut self,
        user_triggered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project() {
            if let Some(buffer) = self.buffer().read(cx).as_singleton()
                && buffer.read(cx).file().is_none()
            {
                return;
            }

            let focused = self.focus_handle(cx).contains_focused(window, cx);

            let project = project.clone();
            let blame = cx
                .new(|cx| GitBlame::new(self.buffer.clone(), project, user_triggered, focused, cx));
            self.blame_subscription =
                Some(cx.observe_in(&blame, window, |_, _, _, cx| cx.notify()));
            self.blame = Some(blame);
        }
    }

    fn restore_hunks_in_ranges(
        &mut self,
        ranges: Vec<Range<Point>>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let hunks = self.snapshot(window, cx).hunks_for_ranges(ranges);
        self.apply_restore(hunks, window, cx);
    }

    fn save_buffers_for_ranges_if_needed(
        &mut self,
        ranges: &[Range<Anchor>],
        cx: &mut Context<Editor>,
    ) -> Task<Result<()>> {
        let multibuffer = self.buffer.read(cx);
        let snapshot = multibuffer.read(cx);
        let buffer_ids: HashSet<_> = ranges
            .iter()
            .flat_map(|range| snapshot.buffer_ids_for_range(range.clone()))
            .collect();
        drop(snapshot);

        let mut buffers = HashSet::default();
        for buffer_id in buffer_ids {
            if let Some(buffer_entity) = multibuffer.buffer(buffer_id) {
                let buffer = buffer_entity.read(cx);
                if buffer.file().is_some_and(|file| file.disk_state().exists()) && buffer.is_dirty()
                {
                    buffers.insert(buffer_entity);
                }
            }
        }

        if let Some(project) = &self.project {
            project.update(cx, |project, cx| project.save_buffers(buffers, cx))
        } else {
            Task::ready(Ok(()))
        }
    }

    fn do_stage_or_unstage_and_next(
        &mut self,
        stage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ranges = self.selections.disjoint_anchor_ranges().collect::<Vec<_>>();

        if ranges.iter().any(|range| range.start != range.end) {
            self.stage_or_unstage_diff_hunks(stage, ranges, window, cx);
            return;
        }

        self.stage_or_unstage_diff_hunks(stage, ranges, window, cx);

        let all_diff_hunks_expanded = self.buffer().read(cx).all_diff_hunks_expanded();
        let wrap_around = !all_diff_hunks_expanded;
        let snapshot = self.snapshot(window, cx);
        let position = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();

        self.go_to_hunk_before_or_after_position(
            &snapshot,
            position,
            Direction::Next,
            wrap_around,
            window,
            cx,
        );
    }

    fn open_git_blame_commit_internal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let blame = self.blame.as_ref()?;
        let snapshot = self.snapshot(window, cx);
        let cursor = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let (buffer, point) = snapshot.buffer_snapshot().point_to_buffer_point(cursor)?;
        let (_, blame_entry) = blame
            .update(cx, |blame, cx| {
                blame
                    .blame_for_rows(
                        &[RowInfo {
                            buffer_id: Some(buffer.remote_id()),
                            buffer_row: Some(point.row),
                            ..Default::default()
                        }],
                        cx,
                    )
                    .next()
            })
            .flatten()?;
        let renderer = cx.global::<GlobalBlameRenderer>().0.clone();
        let repo = blame.read(cx).repository(cx, buffer.remote_id())?;
        let workspace = self.workspace()?.downgrade();
        renderer.open_blame_commit(blame_entry, repo, workspace, window, cx);
        None
    }

    fn has_blame_entries(&self, cx: &App) -> bool {
        self.blame()
            .is_some_and(|blame| blame.read(cx).has_generated_entries())
    }

    fn newest_selection_head_on_empty_line(&self, cx: &App) -> bool {
        let cursor_anchor = self.selections.newest_anchor().head();

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let buffer_row = MultiBufferRow(cursor_anchor.to_point(&snapshot).row);

        snapshot.line_len(buffer_row) == 0
    }
    fn hunk_after_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        wrap_around: bool,
    ) -> Option<MultiBufferDiffHunk> {
        let result = snapshot
            .buffer_snapshot()
            .diff_hunks_in_range(position..snapshot.buffer_snapshot().max_point())
            .find(|hunk| hunk.row_range.start.0 > position.row);

        if wrap_around {
            result.or_else(|| {
                snapshot
                    .buffer_snapshot()
                    .diff_hunks_in_range(Point::zero()..position)
                    .find(|hunk| hunk.row_range.end.0 < position.row)
            })
        } else {
            result
        }
    }

    fn hunk_before_position(
        &mut self,
        snapshot: &EditorSnapshot,
        position: Point,
        wrap_around: bool,
    ) -> Option<MultiBufferRow> {
        let result = snapshot.buffer_snapshot().diff_hunk_before(position);

        if wrap_around {
            result.or_else(|| snapshot.buffer_snapshot().diff_hunk_before(Point::MAX))
        } else {
            result
        }
    }

    /// Dismisses overlays that have no comments stored for their hunks.
    /// Keeps overlays that have at least one comment.
    fn dismiss_overlays_without_comments(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.buffer.read(cx).snapshot(cx);

        // First, compute which overlays have comments (to avoid borrow issues with retain)
        let overlays_with_comments: Vec<bool> = self
            .diff_review_overlays
            .iter()
            .map(|overlay| self.hunk_comment_count(&overlay.hunk_key, &snapshot) > 0)
            .collect();

        // Now collect block IDs to remove and retain overlays
        let mut block_ids_to_remove = HashSet::default();
        let mut index = 0;
        self.diff_review_overlays.retain(|overlay| {
            let has_comments = overlays_with_comments[index];
            index += 1;
            if !has_comments {
                block_ids_to_remove.insert(overlay.block_id);
            }
            has_comments
        });

        if !block_ids_to_remove.is_empty() {
            self.remove_blocks(block_ids_to_remove, None, cx);
            cx.notify();
        }
    }

    pub(super) fn dismiss_stack_review_comment_composers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let overlays_with_comments = self
            .diff_review_overlays
            .iter()
            .map(|overlay| self.hunk_comment_count(&overlay.hunk_key, &snapshot) > 0)
            .collect::<Vec<_>>();
        let mut block_ids_to_remove = HashSet::default();
        let mut hunk_keys_to_resize = Vec::new();
        for (overlay, has_comments) in self
            .diff_review_overlays
            .iter_mut()
            .zip(overlays_with_comments)
        {
            if has_comments {
                if overlay.composer_visible {
                    overlay.composer_visible = false;
                    overlay.pending_reply_to = None;
                    hunk_keys_to_resize.push(overlay.hunk_key.clone());
                }
            } else {
                block_ids_to_remove.insert(overlay.block_id);
            }
        }
        self.diff_review_overlays
            .retain(|overlay| !block_ids_to_remove.contains(&overlay.block_id));
        let removed_empty_overlays = !block_ids_to_remove.is_empty();
        if removed_empty_overlays {
            self.remove_blocks(block_ids_to_remove, None, cx);
        }
        for hunk_key in &hunk_keys_to_resize {
            self.refresh_diff_review_overlay_height(hunk_key, window, cx);
        }
        let changed = removed_empty_overlays || !hunk_keys_to_resize.is_empty();
        if changed {
            cx.notify();
        }
        changed
    }

    /// Refreshes the diff review overlay block to update its height and render function.
    /// Uses resize_blocks and replace_blocks to avoid visual flicker from remove+insert.
    fn refresh_diff_review_overlay_height(
        &mut self,
        hunk_key: &DiffHunkKey,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Extract all needed data from overlay first to avoid borrow conflicts
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let (comments_expanded, composer_visible, block_id, prompt_editor) = {
            let Some(overlay) = self
                .diff_review_overlays
                .iter()
                .find(|overlay| Self::hunk_keys_match(&overlay.hunk_key, hunk_key, &snapshot))
            else {
                return;
            };

            (
                overlay.comments_expanded,
                overlay.composer_visible,
                overlay.block_id,
                overlay.prompt_editor.clone(),
            )
        };

        // Calculate new height
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let new_height =
            self.calculate_overlay_height(hunk_key, comments_expanded, composer_visible, &snapshot);

        // Update the block height using resize_blocks (avoids flicker)
        let mut heights = HashMap::default();
        heights.insert(block_id, new_height);
        self.resize_blocks(heights, None, cx);

        // Update the render function using replace_blocks (avoids flicker)
        let hunk_key_for_render = hunk_key.clone();
        let editor_handle = cx.entity().downgrade();
        let render: Arc<dyn Fn(&mut BlockContext) -> AnyElement + Send + Sync> =
            Arc::new(move |cx| {
                Self::render_diff_review_overlay(
                    &prompt_editor,
                    &hunk_key_for_render,
                    &editor_handle,
                    cx,
                )
            });

        let mut renderers = HashMap::default();
        renderers.insert(block_id, render);
        self.replace_blocks(renderers, None, cx);
    }

    /// Compares two DiffHunkKeys for equality by resolving their anchors.
    fn hunk_keys_match(a: &DiffHunkKey, b: &DiffHunkKey, snapshot: &MultiBufferSnapshot) -> bool {
        a.file_path == b.file_path
            && a.hunk_start_anchor.to_point(snapshot) == b.hunk_start_anchor.to_point(snapshot)
    }

    fn render_diff_review_overlay(
        prompt_editor: &Entity<Editor>,
        hunk_key: &DiffHunkKey,
        editor_handle: &WeakEntity<Editor>,
        cx: &mut BlockContext,
    ) -> AnyElement {
        fn format_line_ranges(ranges: &[(u32, u32)]) -> Option<String> {
            if ranges.is_empty() {
                return None;
            }
            let formatted: Vec<String> = ranges
                .iter()
                .map(|(start, end)| {
                    let start_line = start + 1;
                    let end_line = end + 1;
                    if start_line == end_line {
                        format!("Line {start_line}")
                    } else {
                        format!("Lines {start_line}-{end_line}")
                    }
                })
                .collect();
            // Don't show label for single line in single excerpt
            if ranges.len() == 1 && ranges[0].0 == ranges[0].1 {
                return None;
            }
            Some(formatted.join(" ⋯ "))
        }

        let theme = cx.theme();
        let colors = theme.colors();

        let (
            comments,
            comments_expanded,
            composer_visible,
            pending_reply_to,
            is_stack_review,
            inline_editors,
            user_avatar_uri,
            line_ranges,
        ) = editor_handle
            .upgrade()
            .map(|editor| {
                let editor = editor.read(cx);
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let comments = editor.comments_for_hunk(hunk_key, &snapshot).to_vec();
                let (
                    expanded,
                    composer_visible,
                    pending_reply_to,
                    editors,
                    avatar_uri,
                    line_ranges,
                ) = editor
                    .diff_review_overlays
                    .iter()
                    .find(|overlay| Editor::hunk_keys_match(&overlay.hunk_key, hunk_key, &snapshot))
                    .map(|o| {
                        let start_point = o.anchor_range.start.to_point(&snapshot);
                        let end_point = o.anchor_range.end.to_point(&snapshot);
                        // Get line ranges per excerpt to detect discontinuities
                        let buffer_ranges = snapshot.range_to_buffer_ranges(start_point..end_point);
                        let ranges: Vec<(u32, u32)> = buffer_ranges
                            .iter()
                            .map(|(buffer_snapshot, range, _)| {
                                let start = buffer_snapshot.offset_to_point(range.start.0).row;
                                let end = buffer_snapshot.offset_to_point(range.end.0).row;
                                (start, end)
                            })
                            .collect();
                        (
                            o.comments_expanded,
                            o.composer_visible,
                            o.pending_reply_to,
                            o.inline_edit_editors.clone(),
                            o.user_avatar_uri.clone(),
                            if ranges.is_empty() {
                                None
                            } else {
                                Some(ranges)
                            },
                        )
                    })
                    .unwrap_or((true, true, None, HashMap::default(), None, None));
                (
                    comments,
                    expanded,
                    composer_visible,
                    pending_reply_to,
                    editor.is_stack_review,
                    editors,
                    avatar_uri,
                    line_ranges,
                )
            })
            .unwrap_or((
                Vec::new(),
                true,
                true,
                None,
                false,
                HashMap::default(),
                None,
                None,
            ));

        let comment_count = comments.len();
        let avatar_size = px(20.);
        let action_icon_size = IconSize::XSmall;
        let close_editor = editor_handle.clone();
        let submit_editor = editor_handle.clone();

        v_flex()
            .w_full()
            .bg(colors.editor_background)
            .border_b_1()
            .border_color(colors.border)
            .px_2()
            .pb_2()
            .gap_2()
            // Line range indicator (only shown for multi-line selections or multiple excerpts)
            .when_some(line_ranges, |el, ranges| {
                let label = format_line_ranges(&ranges);
                if let Some(label) = label {
                    el.child(
                        h_flex()
                            .w_full()
                            .px_2()
                            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted)),
                    )
                } else {
                    el
                }
            })
            // Top row: editable input with user's avatar
            .when(composer_visible && pending_reply_to.is_none(), |element| {
                element.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .bg(colors.surface_background)
                        .child(
                            div()
                                .size(avatar_size)
                                .flex_shrink_0()
                                .rounded_full()
                                .overflow_hidden()
                                .child(if let Some(ref avatar_uri) = user_avatar_uri {
                                    Avatar::new(avatar_uri.clone())
                                        .size(avatar_size)
                                        .into_any_element()
                                } else {
                                    Icon::new(IconName::Person)
                                        .size(IconSize::Small)
                                        .color(ui::Color::Muted)
                                        .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .border_1()
                                .border_color(colors.border)
                                .rounded_md()
                                .bg(colors.editor_background)
                                .px_2()
                                .py_1()
                                .child(prompt_editor.clone()),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_1()
                                .child(
                                    IconButton::new("diff-review-close", IconName::Close)
                                        .icon_color(ui::Color::Muted)
                                        .icon_size(action_icon_size)
                                        .tooltip(Tooltip::text("Close"))
                                        .on_click(move |_, window, cx| {
                                            if let Some(editor) = close_editor.upgrade() {
                                                editor.update(cx, |editor, cx| {
                                                    editor.dismiss_stack_review_comment_composers(
                                                        window, cx,
                                                    );
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    IconButton::new("diff-review-add", IconName::Return)
                                        .icon_color(ui::Color::Muted)
                                        .icon_size(action_icon_size)
                                        .tooltip(Tooltip::text("Add comment"))
                                        .on_click(move |_, window, cx| {
                                            if let Some(editor) = submit_editor.upgrade() {
                                                editor.update(cx, |editor, cx| {
                                                    editor.submit_diff_review_comment(window, cx);
                                                });
                                            }
                                        }),
                                ),
                        ),
                )
            })
            // Expandable comments section (only shown when there are comments)
            .when(comment_count > 0, |el| {
                el.child(Self::render_comments_section(
                    comments,
                    comments_expanded,
                    composer_visible,
                    pending_reply_to,
                    is_stack_review,
                    prompt_editor.clone(),
                    inline_editors,
                    user_avatar_uri,
                    avatar_size,
                    action_icon_size,
                    colors,
                    hunk_key.clone(),
                    editor_handle.clone(),
                ))
            })
            .into_any_element()
    }

    fn render_comments_section(
        comments: Vec<StoredReviewComment>,
        expanded: bool,
        composer_visible: bool,
        pending_reply_to: Option<usize>,
        is_stack_review: bool,
        prompt_editor: Entity<Editor>,
        inline_editors: HashMap<usize, Entity<Editor>>,
        user_avatar_uri: Option<SharedUri>,
        avatar_size: Pixels,
        action_icon_size: IconSize,
        colors: &theme::ThemeColors,
        hunk_key: DiffHunkKey,
        editor_handle: WeakEntity<Editor>,
    ) -> impl IntoElement {
        let comment_count = comments.len();
        let expanded = expanded || is_stack_review;
        let thread_items = stack_review_thread_items(
            comments,
            composer_visible.then_some(pending_reply_to).flatten(),
        );
        let editor_handle_for_toggle = editor_handle.clone();
        let hunk_key_for_toggle = hunk_key;

        v_flex()
            .w_full()
            .gap_1()
            // Header with expand/collapse toggle
            .child(
                h_flex()
                    .id("review-comments-header")
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .when(!is_stack_review, move |header| {
                        header
                            .cursor_pointer()
                            .hover(|style| style.bg(colors.ghost_element_hover))
                            .on_click(move |_, window: &mut Window, cx| {
                                if let Some(editor) = editor_handle_for_toggle.upgrade() {
                                    editor.update(cx, |editor, cx| {
                                        editor.toggle_review_comments_for_hunk(
                                            &hunk_key_for_toggle,
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            })
                    })
                    .when(!is_stack_review, |header| {
                        header.child(
                            Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(IconSize::Small)
                            .color(ui::Color::Muted),
                        )
                    })
                    .child(
                        Label::new(format!(
                            "{} Comment{}",
                            comment_count,
                            if comment_count == 1 { "" } else { "s" }
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            // Comments list (when expanded)
            .when(expanded, |el| {
                el.children(thread_items.into_iter().map(|item| {
                    match item {
                        StackReviewThreadItem::Comment { comment, depth } => {
                            let inline_editor = inline_editors.get(&comment.id).cloned();
                            Self::render_comment_row(
                                comment,
                                depth,
                                is_stack_review,
                                inline_editor,
                                user_avatar_uri.clone(),
                                avatar_size,
                                action_icon_size,
                                colors,
                                editor_handle.clone(),
                            )
                            .into_any_element()
                        }
                        StackReviewThreadItem::Composer { depth } => div()
                            .pl(px((depth.min(4) * 12) as f32))
                            .child(Self::render_reply_composer(
                                prompt_editor.clone(),
                                action_icon_size,
                                colors,
                                editor_handle.clone(),
                            ))
                            .into_any_element(),
                    }
                }))
            })
    }

    fn render_reply_composer(
        prompt_editor: Entity<Editor>,
        action_icon_size: IconSize,
        colors: &theme::ThemeColors,
        editor_handle: WeakEntity<Editor>,
    ) -> impl IntoElement {
        let close_editor = editor_handle.clone();
        let submit_editor = editor_handle;
        h_flex()
            .w_full()
            .gap_1()
            .rounded_md()
            .bg(colors.surface_background)
            .border_1()
            .border_color(colors.border)
            .px_2()
            .py_1()
            .child(div().min_w_0().flex_1().child(prompt_editor))
            .child(
                IconButton::new("diff-review-close-reply", IconName::Close)
                    .icon_color(ui::Color::Muted)
                    .icon_size(action_icon_size)
                    .tooltip(Tooltip::text("Cancel reply"))
                    .on_click(move |_, window, cx| {
                        if let Some(editor) = close_editor.upgrade() {
                            editor.update(cx, |editor, cx| {
                                editor.dismiss_stack_review_comment_composers(window, cx);
                            });
                        }
                    }),
            )
            .child(
                IconButton::new("diff-review-submit-reply", IconName::Return)
                    .icon_color(ui::Color::Muted)
                    .icon_size(action_icon_size)
                    .tooltip(Tooltip::text("Add reply"))
                    .on_click(move |_, window, cx| {
                        if let Some(editor) = submit_editor.upgrade() {
                            editor.update(cx, |editor, cx| {
                                editor.submit_diff_review_comment(window, cx);
                            });
                        }
                    }),
            )
    }

    fn render_comment_row(
        comment: StoredReviewComment,
        depth: usize,
        is_stack_review: bool,
        inline_editor: Option<Entity<Editor>>,
        user_avatar_uri: Option<SharedUri>,
        avatar_size: Pixels,
        action_icon_size: IconSize,
        colors: &theme::ThemeColors,
        editor_handle: WeakEntity<Editor>,
    ) -> impl IntoElement {
        let comment_id = comment.id;
        let is_editing = inline_editor.is_some();
        let cancel_editor = editor_handle.clone();
        let confirm_editor = editor_handle.clone();
        let reply_editor = editor_handle.clone();
        let edit_editor = editor_handle.clone();
        let delete_editor = editor_handle.clone();
        let resolution_editor = editor_handle;
        let resolved = comment.resolved;

        let source = comment.source;
        let mut author_label = match source {
            StackReviewCommentSource::LocalHuman => comment.author.name.clone(),
            StackReviewCommentSource::LocalAgent => format!("{} · Agent", comment.author.name),
            StackReviewCommentSource::Github => format!("{} · GitHub", comment.author.name),
        };
        if !comment.created_at_display.is_empty() {
            author_label.push_str(" · ");
            author_label.push_str(&comment.created_at_display);
        }
        let comment_content = if let Some(editor) = inline_editor {
            div()
                .w_full()
                .border_1()
                .border_color(colors.border)
                .rounded_md()
                .bg(colors.editor_background)
                .px_2()
                .py_1()
                .child(editor)
                .into_any_element()
        } else {
            div()
                .w_full()
                .text_sm()
                .text_color(colors.text)
                .child(comment.comment)
                .into_any_element()
        };

        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .bg(colors.surface_background)
            .pl(px(if is_stack_review {
                (depth.min(4) * 12) as f32
            } else {
                0.
            }))
            .child(
                div()
                    .size(avatar_size)
                    .flex_shrink_0()
                    .rounded_full()
                    .overflow_hidden()
                    .child(if source == StackReviewCommentSource::LocalHuman {
                        if let Some(ref avatar_uri) = user_avatar_uri {
                            Avatar::new(avatar_uri.clone())
                                .size(avatar_size)
                                .into_any_element()
                        } else {
                            Icon::new(IconName::Person)
                                .size(IconSize::Small)
                                .color(ui::Color::Muted)
                                .into_any_element()
                        }
                    } else {
                        Icon::new(IconName::Person)
                            .size(IconSize::Small)
                            .color(ui::Color::Muted)
                            .into_any_element()
                    }),
            )
            .child(if is_stack_review {
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(
                        Label::new(author_label)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(comment_content)
                    .into_any_element()
            } else {
                div().flex_1().child(comment_content).into_any_element()
            })
            .child(if is_editing {
                // Editing mode: show close and confirm buttons
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        IconButton::new(
                            format!("diff-review-cancel-edit-{comment_id}"),
                            IconName::Close,
                        )
                        .icon_color(ui::Color::Muted)
                        .icon_size(action_icon_size)
                        .tooltip(Tooltip::text("Cancel"))
                        .on_click(move |_, window, cx| {
                            if let Some(editor) = cancel_editor.upgrade() {
                                editor.update(cx, |editor, cx| {
                                    editor.cancel_edit_review_comment(comment_id, window, cx);
                                });
                            }
                        }),
                    )
                    .child(
                        IconButton::new(
                            format!("diff-review-confirm-edit-{comment_id}"),
                            IconName::Return,
                        )
                        .icon_color(ui::Color::Muted)
                        .icon_size(action_icon_size)
                        .tooltip(Tooltip::text("Confirm"))
                        .on_click(move |_, window, cx| {
                            if let Some(editor) = confirm_editor.upgrade() {
                                editor.update(cx, |editor, cx| {
                                    editor.confirm_edit_review_comment(comment_id, window, cx);
                                });
                            }
                        }),
                    )
                    .into_any_element()
            } else if is_stack_review {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        IconButton::new(
                            format!("diff-review-resolution-{comment_id}"),
                            if resolved {
                                IconName::Undo
                            } else {
                                IconName::Check
                            },
                        )
                        .icon_color(ui::Color::Muted)
                        .icon_size(action_icon_size)
                        .tooltip(Tooltip::text(if resolved {
                            "Reopen thread"
                        } else {
                            "Resolve thread"
                        }))
                        .on_click(move |_, _, cx| {
                            if let Some(editor) = resolution_editor.upgrade() {
                                editor.update(cx, |editor, cx| {
                                    editor.set_review_comment_resolved(comment_id, !resolved, cx);
                                });
                            }
                        }),
                    )
                    .child(
                        IconButton::new(
                            format!("diff-review-reply-{comment_id}"),
                            IconName::ReplyArrowRight,
                        )
                        .icon_color(ui::Color::Muted)
                        .icon_size(action_icon_size)
                        .tooltip(Tooltip::text("Reply"))
                        .on_click(move |_, window, cx| {
                            if let Some(editor) = reply_editor.upgrade() {
                                editor.update(cx, |editor, cx| {
                                    editor.reply_to_review_comment(
                                        &ReplyToReviewComment { id: comment_id },
                                        window,
                                        cx,
                                    );
                                });
                            }
                        }),
                    )
                    .when(source != StackReviewCommentSource::Github, |actions| {
                        actions
                            .child(
                                IconButton::new(
                                    format!("diff-review-edit-{comment_id}"),
                                    IconName::Pencil,
                                )
                                .icon_color(ui::Color::Muted)
                                .icon_size(action_icon_size)
                                .tooltip(Tooltip::text("Edit"))
                                .on_click(move |_, window, cx| {
                                    if let Some(editor) = edit_editor.upgrade() {
                                        editor.update(cx, |editor, cx| {
                                            editor.edit_review_comment(
                                                &EditReviewComment { id: comment_id },
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                            )
                            .child(
                                IconButton::new(
                                    format!("diff-review-delete-{comment_id}"),
                                    IconName::Trash,
                                )
                                .icon_color(ui::Color::Muted)
                                .icon_size(action_icon_size)
                                .tooltip(Tooltip::text("Delete"))
                                .on_click(move |_, window, cx| {
                                    if let Some(editor) = delete_editor.upgrade() {
                                        editor.update(cx, |editor, cx| {
                                            editor.delete_review_comment(
                                                &DeleteReviewComment { id: comment_id },
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                            )
                    })
                    .into_any_element()
            } else {
                gpui::Empty.into_any_element()
            })
    }

    fn get_permalink_to_line(&self, cx: &mut Context<Self>) -> Task<Result<url::Url>> {
        let buffer_and_selection = maybe!({
            let selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
            let selection_range = selection.range();

            let multi_buffer = self.buffer().read(cx);
            let multi_buffer_snapshot = multi_buffer.snapshot(cx);
            let buffer_ranges = multi_buffer_snapshot
                .range_to_buffer_ranges(selection_range.start..selection_range.end);

            let (buffer_snapshot, range, _) = if selection.reversed {
                buffer_ranges.first()
            } else {
                buffer_ranges.last()
            }?;

            let buffer_range = range.to_point(buffer_snapshot);
            let buffer = multi_buffer.buffer(buffer_snapshot.remote_id())?;

            let Some(buffer_diff) = multi_buffer.diff_for(buffer_snapshot.remote_id()) else {
                return Some((buffer, buffer_range.start.row..buffer_range.end.row));
            };

            let buffer_diff_snapshot = buffer_diff.read(cx).snapshot(cx);
            let start = buffer_diff_snapshot
                .buffer_point_to_base_text_point(buffer_range.start, &buffer_snapshot);
            let end = buffer_diff_snapshot
                .buffer_point_to_base_text_point(buffer_range.end, &buffer_snapshot);

            Some((buffer, start.row..end.row))
        });

        let Some((buffer, selection)) = buffer_and_selection else {
            return Task::ready(Err(anyhow!("failed to determine buffer and selection")));
        };

        let Some(project) = self.project() else {
            return Task::ready(Err(anyhow!("editor does not have project")));
        };

        project.update(cx, |project, cx| {
            project.get_permalink_to_line(&buffer, selection, cx)
        })
    }
}

#[cfg(test)]
impl Editor {
    /// Returns the line range for the first diff review overlay, if one is active.
    /// Returns (start_row, end_row) as physical line numbers in the underlying file.
    pub(super) fn diff_review_line_range(&self, cx: &App) -> Option<(u32, u32)> {
        let overlay = self.diff_review_overlays.first()?;
        let snapshot = self.buffer.read(cx).snapshot(cx);
        let start_point = overlay.anchor_range.start.to_point(&snapshot);
        let end_point = overlay.anchor_range.end.to_point(&snapshot);
        let start_row = snapshot
            .point_to_buffer_point(start_point)
            .map(|(_, p)| p.row)
            .unwrap_or(start_point.row);
        let end_row = snapshot
            .point_to_buffer_point(end_point)
            .map(|(_, p)| p.row)
            .unwrap_or(end_point.row);
        Some((start_row, end_row))
    }

    /// Takes all stored comments from all hunks, clearing the storage.
    /// Returns a Vec of (hunk_key, comments) pairs.
    pub(super) fn take_all_review_comments(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<(DiffHunkKey, Vec<StoredReviewComment>)> {
        // Dismiss all overlays when taking comments (e.g., when sending to agent)
        self.dismiss_all_diff_review_overlays(cx);
        let comments = std::mem::take(&mut self.stored_review_comments);
        // Reset the ID counter since all comments have been taken
        self.next_review_comment_id = 0;
        cx.emit(EditorEvent::ReviewCommentsChanged { total_count: 0 });
        cx.notify();
        comments
    }
}

impl EditorSnapshot {
    pub(super) fn display_diff_hunks_for_rows<'a>(
        &'a self,
        display_rows: Range<DisplayRow>,
        folded_buffers: &'a HashSet<BufferId>,
    ) -> impl 'a + Iterator<Item = DisplayDiffHunk> {
        let buffer_start = DisplayPoint::new(display_rows.start, 0).to_point(self);
        let buffer_end = DisplayPoint::new(display_rows.end, 0).to_point(self);

        self.buffer_snapshot()
            .diff_hunks_in_range(buffer_start..buffer_end)
            .filter_map(|hunk| {
                if folded_buffers.contains(&hunk.buffer_id)
                    || (hunk.row_range.is_empty() && self.buffer.all_diff_hunks_expanded())
                {
                    return None;
                }

                let hunk_start_point = Point::new(hunk.row_range.start.0, 0);
                let hunk_end_point = if hunk.row_range.end > hunk.row_range.start {
                    let last_row = MultiBufferRow(hunk.row_range.end.0 - 1);
                    let line_len = self.buffer_snapshot().line_len(last_row);
                    Point::new(last_row.0, line_len)
                } else {
                    Point::new(hunk.row_range.end.0, 0)
                };

                let hunk_display_start = self.point_to_display_point(hunk_start_point, Bias::Left);
                let hunk_display_end = self.point_to_display_point(hunk_end_point, Bias::Right);

                let display_hunk = if hunk_display_start.column() != 0 {
                    DisplayDiffHunk::Folded {
                        display_row: hunk_display_start.row(),
                    }
                } else {
                    let mut end_row = hunk_display_end.row();
                    if hunk.row_range.end > hunk.row_range.start || hunk_display_end.column() > 0 {
                        end_row.0 += 1;
                    }
                    let is_created_file = hunk.is_created_file();
                    let multi_buffer_range = hunk.multi_buffer_range.clone();

                    DisplayDiffHunk::Unfolded {
                        status: hunk.status(),
                        diff_base_byte_range: hunk.diff_base_byte_range.start.0
                            ..hunk.diff_base_byte_range.end.0,
                        word_diffs: hunk.word_diffs,
                        display_row_range: hunk_display_start.row()..end_row,
                        multi_buffer_range,
                        is_created_file,
                    }
                };

                Some(display_hunk)
            })
    }

    fn hunks_for_ranges(
        &self,
        ranges: impl IntoIterator<Item = Range<Point>>,
    ) -> Vec<MultiBufferDiffHunk> {
        let mut hunks = Vec::new();
        let mut processed_buffer_rows: HashMap<BufferId, HashSet<Range<text::Anchor>>> =
            HashMap::default();
        for query_range in ranges {
            let query_rows =
                MultiBufferRow(query_range.start.row)..MultiBufferRow(query_range.end.row + 1);
            for hunk in self.buffer_snapshot().diff_hunks_in_range(
                Point::new(query_rows.start.0, 0)..Point::new(query_rows.end.0, 0),
            ) {
                // Include deleted hunks that are adjacent to the query range, because
                // otherwise they would be missed.
                let mut intersects_range = hunk.row_range.overlaps(&query_rows);
                if hunk.status().is_deleted() {
                    intersects_range |= hunk.row_range.start == query_rows.end;
                    intersects_range |= hunk.row_range.end == query_rows.start;
                }
                if intersects_range {
                    if !processed_buffer_rows
                        .entry(hunk.buffer_id)
                        .or_default()
                        .insert(hunk.buffer_range.start..hunk.buffer_range.end)
                    {
                        continue;
                    }
                    hunks.push(hunk);
                }
            }
        }

        hunks
    }
}

pub fn set_blame_renderer(renderer: impl BlameRenderer + 'static, cx: &mut App) {
    cx.set_global(GlobalBlameRenderer(Arc::new(renderer)));
}

pub fn render_diff_hunk_controls(
    row: u32,
    status: &DiffHunkStatus,
    hunk_range: Range<Anchor>,
    is_created_file: bool,
    line_height: Pixels,
    editor: &Entity<Editor>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let stageable = hunk_range
        .start
        .buffer_id()
        .and_then(|buffer_id| editor.read(cx).buffer().read(cx).diff_for(buffer_id))
        .is_some_and(|diff| diff.read(cx).is_stageable());
    let show_stage_restore = stageable
        && ProjectSettings::get_global(cx)
            .git
            .show_stage_restore_buttons;

    h_flex()
        .h(line_height)
        .mr_1()
        .gap_1()
        .px_0p5()
        .pb_1()
        .border_x_1()
        .border_b_1()
        .border_color(cx.theme().colors().border_variant)
        .rounded_b_lg()
        .bg(cx.theme().colors().editor_background)
        .gap_1()
        .block_mouse_except_scroll()
        .shadow_md()
        .when(show_stage_restore, |el| {
            el.child(if status.has_secondary_hunk() {
                Button::new(("stage", row as u64), "Stage")
                    .alpha(if status.is_pending() { 0.66 } else { 1.0 })
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Stage Hunk",
                                &::git::ToggleStaged,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.stage_or_unstage_diff_hunks(
                                    true,
                                    vec![hunk_range.start..hunk_range.start],
                                    window,
                                    cx,
                                );
                            });
                        }
                    })
            } else {
                Button::new(("unstage", row as u64), "Unstage")
                    .alpha(if status.is_pending() { 0.66 } else { 1.0 })
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Unstage Hunk",
                                &::git::ToggleStaged,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                editor.stage_or_unstage_diff_hunks(
                                    false,
                                    vec![hunk_range.start..hunk_range.start],
                                    window,
                                    cx,
                                );
                            });
                        }
                    })
            })
        })
        .when(show_stage_restore, |el| {
            el.child(
                Button::new(("restore", row as u64), "Restore")
                    .tooltip({
                        let focus_handle = editor.focus_handle(cx);
                        move |_window, cx| {
                            Tooltip::for_action_in(
                                "Restore Hunk",
                                &::git::Restore,
                                &focus_handle,
                                cx,
                            )
                        }
                    })
                    .on_click({
                        let editor = editor.clone();
                        move |_event, window, cx| {
                            editor.update(cx, |editor, cx| {
                                let snapshot = editor.snapshot(window, cx);
                                let point = hunk_range.start.to_point(&snapshot.buffer_snapshot());
                                editor.restore_hunks_in_ranges(vec![point..point], window, cx);
                            });
                        }
                    })
                    .disabled(is_created_file),
            )
        })
        .when(
            !editor.read(cx).buffer().read(cx).all_diff_hunks_expanded(),
            |el| {
                el.child(
                    IconButton::new(("next-hunk", row as u64), IconName::ArrowDown)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        // .disabled(!has_multiple_hunks)
                        .tooltip({
                            let focus_handle = editor.focus_handle(cx);
                            move |_window, cx| {
                                Tooltip::for_action_in("Next Hunk", &GoToHunk, &focus_handle, cx)
                            }
                        })
                        .on_click({
                            let editor = editor.clone();
                            move |_event, window, cx| {
                                editor.update(cx, |editor, cx| {
                                    let snapshot = editor.snapshot(window, cx);
                                    let position =
                                        hunk_range.end.to_point(&snapshot.buffer_snapshot());
                                    editor.go_to_hunk_before_or_after_position(
                                        &snapshot,
                                        position,
                                        Direction::Next,
                                        true,
                                        window,
                                        cx,
                                    );
                                    editor.expand_selected_diff_hunks(cx);
                                });
                            }
                        }),
                )
                .child(
                    IconButton::new(("prev-hunk", row as u64), IconName::ArrowUp)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        // .disabled(!has_multiple_hunks)
                        .tooltip({
                            let focus_handle = editor.focus_handle(cx);
                            move |_window, cx| {
                                Tooltip::for_action_in(
                                    "Previous Hunk",
                                    &GoToPreviousHunk,
                                    &focus_handle,
                                    cx,
                                )
                            }
                        })
                        .on_click({
                            let editor = editor.clone();
                            move |_event, window, cx| {
                                editor.update(cx, |editor, cx| {
                                    let snapshot = editor.snapshot(window, cx);
                                    let point =
                                        hunk_range.start.to_point(&snapshot.buffer_snapshot());
                                    editor.go_to_hunk_before_or_after_position(
                                        &snapshot,
                                        point,
                                        Direction::Prev,
                                        true,
                                        window,
                                        cx,
                                    );
                                    editor.expand_selected_diff_hunks(cx);
                                });
                            }
                        }),
                )
            },
        )
        .into_any_element()
}

impl Editor {
    pub(super) fn update_uncommitted_diff_for_buffer(
        &mut self,
        project: &Entity<Project>,
        buffers: impl IntoIterator<Item = Entity<Buffer>>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let mut tasks = Vec::new();
        project.update(cx, |project, cx| {
            let git_store = project.git_store().clone();
            git_store.update(cx, |git_store, cx| {
                for buffer in buffers {
                    if project::File::from_dyn(buffer.read(cx).file()).is_some() {
                        tasks.push(git_store.open_display_diff(buffer, cx));
                    }
                }
            });
        });

        let editor = cx.entity();
        let buffer = self.buffer.clone();
        cx.spawn(async move |_, cx| {
            let diffs = future::join_all(tasks).await;
            if editor.read_with(cx, |editor, _cx| editor.diff_hunk_delegate.is_some()) {
                return;
            }

            buffer.update(cx, |buffer, cx| {
                for diff in diffs.into_iter().flatten() {
                    buffer.add_diff(diff, cx);
                }
            });
        })
    }
}
