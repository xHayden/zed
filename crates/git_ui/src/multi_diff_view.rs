use anyhow::{Context as _, Result};
use buffer_diff::BufferDiff;
use editor::{
    Editor, EditorEvent, MultiBuffer, RestoreOnlyUnstagedDiffHunkDelegate, SplittableEditor,
    multibuffer_context_lines,
};
use git_ui_core::file_diff_view::build_buffer_diff;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Font, IntoElement, ParentElement, Render, SharedString, Styled, Task, Window,
};
use language::{Buffer, Capability, HighlightedText, OffsetRangeExt};
use multi_buffer::PathKey;
use project::{Project, ProjectPath};
use settings::DiffViewStyle;
use std::{
    any::{Any, TypeId},
    path::{Path, PathBuf},
    sync::Arc,
};
use ui::{Color, Icon, IconName, Label, LabelCommon as _};
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::{
    Item, ItemHandle as _, ItemNavHistory, ToolbarItemLocation, Workspace,
    item::{ItemEvent, SaveOptions, TabContentParams},
    searchable::SearchableItemHandle,
};

pub struct MultiDiffView {
    editor: Entity<Editor>,
    split_editor: Option<Entity<SplittableEditor>>,
    file_count: usize,
}

#[derive(Clone)]
pub(crate) struct ContentDiffEntry {
    pub path: PathBuf,
    pub source_path: Option<PathBuf>,
    pub was_deleted: bool,
    pub old_text: Arc<str>,
    pub new_text: Arc<str>,
}

struct Entry {
    index: usize,
    new_path: PathBuf,
    new_buffer: Entity<Buffer>,
    diff: Entity<BufferDiff>,
}

async fn load_entries(
    diff_pairs: Vec<[String; 2]>,
    project: &Entity<Project>,
    cx: &mut AsyncApp,
) -> Result<(Vec<Entry>, Option<PathBuf>)> {
    let mut entries = Vec::with_capacity(diff_pairs.len());
    let mut all_paths = Vec::with_capacity(diff_pairs.len());

    for (ix, pair) in diff_pairs.into_iter().enumerate() {
        let old_path = PathBuf::from(&pair[0]);
        let new_path = PathBuf::from(&pair[1]);

        let old_buffer = project
            .update(cx, |project, cx| project.open_local_buffer(&old_path, cx))
            .await?;
        let new_buffer = project
            .update(cx, |project, cx| project.open_local_buffer(&new_path, cx))
            .await?;

        let diff = build_buffer_diff(&old_buffer, &new_buffer, cx).await?;

        all_paths.push(new_path.clone());
        entries.push(Entry {
            index: ix,
            new_path,
            new_buffer: new_buffer.clone(),
            diff,
        });
    }

    let common_root = common_prefix(&all_paths);
    Ok((entries, common_root))
}

async fn load_content_entries(
    content_entries: Vec<ContentDiffEntry>,
    project: &Entity<Project>,
    cx: &mut AsyncApp,
) -> Result<(Vec<Entry>, Option<PathBuf>)> {
    let mut entries = Vec::with_capacity(content_entries.len());
    let mut all_paths = Vec::with_capacity(content_entries.len());
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());

    for (index, entry) in content_entries.into_iter().enumerate() {
        let path = entry.path;
        let language = language_registry
            .load_language_for_file_path(&path)
            .await
            .ok();
        let file = if let Some(source_path) = &entry.source_path {
            Some(
                project
                    .read_with(cx, |project, cx| {
                        project.historic_file_for_absolute_path(source_path, entry.was_deleted, cx)
                    })
                    .with_context(|| {
                        format!("missing project file identity for {source_path:?}")
                    })?,
            )
        } else {
            None
        };
        let (old_buffer, new_buffer) = cx.update(|cx| {
            let old_buffer = cx.new(|cx| {
                let mut buffer = Buffer::local(entry.old_text.as_ref().to_owned(), cx);
                buffer.set_language_registry(language_registry.clone());
                buffer.set_language(language.clone(), cx);
                if let Some(file) = file.clone() {
                    buffer.file_updated(file, cx);
                }
                buffer.set_capability(Capability::ReadOnly, cx);
                buffer
            });
            let new_buffer = cx.new(|cx| {
                let mut buffer = Buffer::local(entry.new_text.as_ref().to_owned(), cx);
                buffer.set_language_registry(language_registry.clone());
                buffer.set_language(language, cx);
                if let Some(file) = file {
                    buffer.file_updated(file, cx);
                }
                buffer.set_capability(Capability::ReadOnly, cx);
                buffer
            });
            (old_buffer, new_buffer)
        });
        let diff = build_buffer_diff(&old_buffer, &new_buffer, cx).await?;

        all_paths.push(path.clone());
        entries.push(Entry {
            index,
            new_path: path,
            new_buffer,
            diff,
        });
    }

    let common_root = common_prefix(&all_paths);
    Ok((entries, common_root))
}

fn register_entry(
    multibuffer: &Entity<MultiBuffer>,
    entry: Entry,
    common_root: &Option<PathBuf>,
    context_lines: u32,
    full_file: bool,
    cx: &mut App,
) {
    let snapshot = entry.new_buffer.read(cx).snapshot();
    let diff_snapshot = entry.diff.read(cx).snapshot(cx);

    let ranges: Vec<std::ops::Range<language::Point>> = if full_file {
        vec![language::Point::zero()..snapshot.max_point()]
    } else {
        diff_snapshot
            .hunks(&snapshot)
            .map(|hunk| hunk.buffer_range.to_point(&snapshot))
            .collect()
    };

    let display_rel = common_root
        .as_ref()
        .and_then(|root| entry.new_path.strip_prefix(root).ok())
        .map(|rel| {
            RelPath::new(rel, PathStyle::local())
                .map(|r| r.into_owned().into())
                .unwrap_or_else(|_| {
                    RelPath::new(Path::new(MultiBuffer::DEFAULT_TITLE), PathStyle::Unix)
                        .unwrap()
                        .into_owned()
                        .into()
                })
        })
        .unwrap_or_else(|| {
            entry
                .new_path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|s| RelPath::new(Path::new(s), PathStyle::Unix).ok())
                .map(|r| r.into_owned().into())
                .unwrap_or_else(|| {
                    RelPath::new(Path::new(MultiBuffer::DEFAULT_TITLE), PathStyle::Unix)
                        .unwrap()
                        .into_owned()
                        .into()
                })
        });

    let path_key = PathKey::with_sort_prefix(entry.index as u64, display_rel);

    multibuffer.update(cx, |multibuffer, cx| {
        multibuffer.set_excerpts_for_path(
            path_key,
            entry.new_buffer.clone(),
            ranges,
            context_lines,
            cx,
        );
        multibuffer.add_diff(entry.diff.clone(), cx);
    });
}

fn common_prefix(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let mut prefix = iter.next()?.clone();

    for path in iter {
        while !path.starts_with(&prefix) {
            if !prefix.pop() {
                return Some(PathBuf::new());
            }
        }
    }

    Some(prefix)
}

impl MultiDiffView {
    pub(crate) fn build_from_content(
        content_entries: Vec<ContentDiffEntry>,
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let context_lines = multibuffer_context_lines(cx);
        window.spawn(cx, async move |cx| {
            let (entries, common_root) =
                load_content_entries(content_entries, &project, cx).await?;
            cx.update(|window, cx| {
                let multibuffer = cx.new(|cx| {
                    let mut multibuffer = MultiBuffer::new(Capability::ReadOnly);
                    multibuffer.set_all_diff_hunks_expanded(cx);
                    multibuffer
                });
                let file_count = entries.len();
                for entry in entries {
                    register_entry(&multibuffer, entry, &common_root, context_lines, true, cx);
                }
                let split_editor = cx.new(|cx| {
                    SplittableEditor::new(
                        DiffViewStyle::Split,
                        multibuffer,
                        project,
                        workspace,
                        window,
                        cx,
                    )
                });
                let editor = split_editor.read(cx).rhs_editor().clone();
                let view = cx.new(|_| Self {
                    editor,
                    split_editor: Some(split_editor),
                    file_count,
                });
                view.update(cx, |view, cx| {
                    view.editor.update(cx, |editor, cx| {
                        editor.set_show_diff_review_button(true, cx);
                        editor.set_stack_review_mode(true, cx);
                        editor.set_allow_git_diff_scrollbar_markers(true, cx);
                        editor.set_minimap_visibility(
                            editor::MinimapVisibility::Enabled {
                                setting_configuration: true,
                                toggle_override: false,
                            },
                            window,
                            cx,
                        );
                    });
                });
                view
            })
        })
    }

    pub(crate) fn editor(&self) -> Entity<Editor> {
        self.editor.clone()
    }

    pub(crate) fn split_left_ratio(&self, cx: &App) -> f32 {
        self.split_editor
            .as_ref()
            .map(|split_editor| split_editor.read(cx).split_left_ratio(cx))
            .unwrap_or(0.5)
    }

    pub(crate) fn set_split_left_ratio(&self, ratio: f32, cx: &mut Context<Self>) {
        if let Some(split_editor) = &self.split_editor {
            split_editor.update(cx, |split_editor, cx| {
                split_editor.set_split_left_ratio(ratio, cx);
            });
        }
    }

    pub(crate) fn searchable_handle(&self) -> Box<dyn SearchableItemHandle> {
        if let Some(split_editor) = &self.split_editor {
            Box::new(split_editor.clone())
        } else {
            Box::new(self.editor.clone())
        }
    }

    #[cfg(test)]
    pub(crate) fn is_split(&self, cx: &App) -> bool {
        self.split_editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).is_split())
    }

    pub fn open(
        diff_pairs: Vec<[String; 2]>,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let project = workspace.project().clone();
        let workspace = workspace.weak_handle();
        let context_lines = multibuffer_context_lines(cx);

        window.spawn(cx, async move |cx| {
            let (entries, common_root) = load_entries(diff_pairs, &project, cx).await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let multibuffer = cx.new(|cx| {
                    let mut multibuffer = MultiBuffer::new(Capability::ReadWrite);
                    multibuffer.set_all_diff_hunks_expanded(cx);
                    multibuffer
                });

                let file_count = entries.len();
                for entry in entries {
                    register_entry(&multibuffer, entry, &common_root, context_lines, false, cx);
                }

                let diff_view = cx.new(|cx| {
                    Self::new(multibuffer.clone(), project.clone(), file_count, window, cx)
                });

                let pane = workspace.active_pane();
                pane.update(cx, |pane, cx| {
                    pane.add_item(Box::new(diff_view.clone()), true, true, None, window, cx);
                });

                // Hide the left dock (file explorer) for a cleaner diff view
                workspace.left_dock().update(cx, |dock, cx| {
                    dock.set_open(false, window, cx);
                });

                diff_view
            })
        })
    }

    fn new(
        multibuffer: Entity<MultiBuffer>,
        project: Entity<Project>,
        file_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor =
                Editor::for_multibuffer(multibuffer, Some(project.clone()), window, cx);
            editor.set_diff_hunk_delegate(Some(Arc::new(RestoreOnlyUnstagedDiffHunkDelegate)), cx);
            editor.disable_diagnostics(cx);
            editor.set_expand_all_diff_hunks(cx);
            editor
        });

        Self {
            editor,
            split_editor: None,
            file_count,
        }
    }

    fn title(&self) -> SharedString {
        let suffix = if self.file_count == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", self.file_count)
        };
        format!("Diff ({suffix})").into()
    }
}

impl EventEmitter<EditorEvent> for MultiDiffView {}

impl Focusable for MultiDiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.split_editor
            .as_ref()
            .map(|editor| editor.focus_handle(cx))
            .unwrap_or_else(|| self.editor.focus_handle(cx))
    }
}

impl Item for MultiDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
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

    fn tab_tooltip_text(&self, _cx: &App) -> Option<ui::SharedString> {
        Some(self.title())
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title()
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Diff View Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(split_editor) = &self.split_editor {
            split_editor.update(cx, |editor, cx| editor.deactivated(window, cx));
        } else {
            self.editor
                .update(cx, |editor, cx| editor.deactivated(window, cx));
        }
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<SplittableEditor>() {
            self.split_editor.clone().map(Into::into)
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(self.searchable_handle())
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.editor.read(cx).active_project_path(cx)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(split_editor) = &self.split_editor {
            split_editor.update(cx, |editor, cx| editor.navigate(data, window, cx))
        } else {
            self.editor
                .update(cx, |editor, cx| editor.navigate(data, window, cx))
        }
    }

    fn breadcrumb_location(&self, _: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(&self, cx: &App) -> Option<(Vec<HighlightedText>, Option<Font>)> {
        self.editor.breadcrumbs(cx)
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(split_editor) = &self.split_editor {
            split_editor.update(cx, |editor, cx| {
                editor.added_to_workspace(workspace, window, cx)
            });
        } else {
            self.editor.update(cx, |editor, cx| {
                editor.added_to_workspace(workspace, window, cx)
            });
        }
    }

    fn can_save(&self, cx: &App) -> bool {
        self.editor.read(cx).can_save(cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<()>> {
        self.editor
            .update(cx, |editor, cx| editor.save(options, project, window, cx))
    }
}

impl Render for MultiDiffView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div().size_full().child(
            self.split_editor
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| self.editor.clone().into_any_element()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::ToPoint as _;
    use fs::Fs as _;
    use gpui::{TestAppContext, VisualTestContext};
    use language::language_settings::AllLanguageSettings;
    use project::{FakeFs, Project, WorktreeSettings, project_settings::ProjectSettings};
    use serde_json::json;
    use settings::{Settings as _, SettingsStore};
    use std::path::Path;
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
    async fn content_diff_buffers_keep_file_identity(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            "/project",
            json!({
                ".git": {},
                "src": { "main.rs": "fn main() {}" }
            }),
        )
        .await;
        let project = Project::test(fs, [Path::new("/project")], cx).await;
        project.read_with(cx, |project, _| {
            project.languages().add(language::rust_lang());
        });
        let workspace =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut visual_context = VisualTestContext::from_window(*workspace, cx);

        let task = workspace
            .update(&mut visual_context, |_workspace, window, cx| {
                MultiDiffView::build_from_content(
                    vec![ContentDiffEntry {
                        path: PathBuf::from("src/main.rs"),
                        source_path: Some(PathBuf::from("/project/src/main.rs")),
                        was_deleted: false,
                        old_text: "fn old() {}".into(),
                        new_text: "fn main() {}".into(),
                    }],
                    project,
                    cx.entity(),
                    window,
                    cx,
                )
            })
            .expect("update workspace");
        let view = task.await.expect("build content diff");
        visual_context.run_until_parked();
        assert!(view.read_with(&visual_context, |view, cx| view.is_split(cx)));
        let (file_path, language_name) = view.read_with(&visual_context, |view, cx| {
            let editor = view.editor();
            let editor = editor.read(cx);
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let point = snapshot
                .diff_hunks_in_range(language::Point::zero()..snapshot.max_point())
                .next()
                .map(|hunk| hunk.multi_buffer_range.start.to_point(&snapshot));
            let file_path = point
                .and_then(|point| editor.review_file_path_at(point, cx))
                .map(|path| path.as_unix_str().to_owned());
            let language_name =
                editor
                    .buffer()
                    .read(cx)
                    .all_buffers_iter()
                    .next()
                    .and_then(|buffer| {
                        buffer
                            .read(cx)
                            .language()
                            .map(|language| language.name().to_string())
                    });
            (file_path, language_name)
        });

        assert_eq!(file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(language_name.as_deref(), Some("Rust"));

        let persisted_comment = git::stack_review::StackReviewComment {
            id: 11,
            path: "src/main.rs".into(),
            start_row: 0,
            start_column: 0,
            end_row: 0,
            end_column: 4,
            body: "Preserve this review note".into(),
            created_at: "2026-08-21T12:00:00Z".into(),
            resolved: false,
            author: git::stack_review::StackReviewCommentAuthor {
                name: "Reviewer".into(),
                login: Some("reviewer".into()),
            },
            source: git::stack_review::StackReviewCommentSource::LocalHuman,
            reply_to: None,
        };
        let persisted_reply = git::stack_review::StackReviewComment {
            id: 12,
            path: "src/main.rs".into(),
            start_row: 0,
            start_column: 0,
            end_row: 0,
            end_column: 4,
            body: "Agent reply".into(),
            created_at: "2026-08-21T12:01:00Z".into(),
            resolved: false,
            author: git::stack_review::StackReviewCommentAuthor {
                name: "Claude Code".into(),
                login: None,
            },
            source: git::stack_review::StackReviewCommentSource::LocalAgent,
            reply_to: Some(11),
        };
        let editor = view.read_with(&visual_context, |view, _| view.editor());
        editor.update_in(&mut visual_context, |editor, window, cx| {
            editor.restore_stack_review_comments(
                &[persisted_comment.clone(), persisted_reply.clone()],
                cx,
            );
            editor.reveal_restored_stack_review_comments(window, cx);
        });
        let (restored, visible_count, has_visible_overlay) =
            editor.read_with(&visual_context, |editor, cx| {
                (
                    editor.stack_review_comments(cx),
                    editor.visible_stack_review_comment_count(cx),
                    editor.diff_review_prompt_editor().is_some(),
                )
            });

        assert_eq!(restored, vec![persisted_comment, persisted_reply]);
        assert_eq!(visible_count, 2);
        assert!(has_visible_overlay);
    }

    #[test]
    fn content_diff_entry_clones_share_revision_text() {
        let entry = ContentDiffEntry {
            path: PathBuf::from("large.rs"),
            source_path: None,
            was_deleted: false,
            old_text: Arc::from("old"),
            new_text: Arc::from("new"),
        };
        let cloned = entry.clone();
        assert!(Arc::ptr_eq(&entry.old_text, &cloned.old_text));
        assert!(Arc::ptr_eq(&entry.new_text, &cloned.new_text));
    }

    #[gpui::test]
    async fn content_diff_accepts_a_binary_source_with_placeholder_text(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            "/project",
            json!({
                ".git": {}
            }),
        )
        .await;
        fs.write(Path::new("/project/image.webp"), &[0, 1, 2, 3])
            .await
            .expect("write binary fixture");
        let project = Project::test(fs, [Path::new("/project")], cx).await;
        let workspace =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut visual_context = VisualTestContext::from_window(*workspace, cx);

        let task = workspace
            .update(&mut visual_context, |_workspace, window, cx| {
                MultiDiffView::build_from_content(
                    vec![ContentDiffEntry {
                        path: PathBuf::from("image.webp"),
                        source_path: None,
                        was_deleted: false,
                        old_text: Arc::from(""),
                        new_text: "Binary file added; content not shown\n".into(),
                    }],
                    project,
                    cx.entity(),
                    window,
                    cx,
                )
            })
            .expect("update workspace");

        assert!(task.await.is_ok());
    }

    #[gpui::test]
    async fn content_diff_accepts_a_deleted_historical_source(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree("/project", json!({ ".git": {} })).await;
        let project = Project::test(fs, [Path::new("/project")], cx).await;
        let workspace =
            cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut visual_context = VisualTestContext::from_window(*workspace, cx);
        let task = workspace
            .update(&mut visual_context, |_workspace, window, cx| {
                MultiDiffView::build_from_content(
                    vec![ContentDiffEntry {
                        path: PathBuf::from("deleted.rs"),
                        source_path: Some(PathBuf::from("/project/deleted.rs")),
                        was_deleted: true,
                        old_text: "fn deleted() {}".into(),
                        new_text: Arc::from(""),
                    }],
                    project,
                    cx.entity(),
                    window,
                    cx,
                )
            })
            .expect("update workspace");

        assert!(task.await.is_ok());
    }
}
