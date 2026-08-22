use std::{fmt, ops::Range, rc::Rc, sync::Arc};

use git::stack_review::StackReviewCommentSide;
use gpui::{App, Context, Entity, Global, SharedString};
use markdown::Markdown;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StackReviewAiGeneration(u64);

impl StackReviewAiGeneration {
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackReviewAiStatus {
    Loading,
    Ready,
    Generating,
    Canceled,
    Failed(SharedString),
    MissingThread,
    NoModel,
}

#[derive(Clone)]
pub struct StackReviewAiTurn {
    envelope: StackReviewTurnEnvelope,
    markdown: Entity<Markdown>,
}

impl StackReviewAiTurn {
    pub fn from_envelope(envelope: StackReviewTurnEnvelope, markdown: Entity<Markdown>) -> Self {
        Self { envelope, markdown }
    }

    pub fn envelope(&self) -> &StackReviewTurnEnvelope {
        &self.envelope
    }

    pub fn turn_id(&self) -> &SharedString {
        self.envelope.turn_id()
    }

    pub fn project_identity(&self) -> &SharedString {
        self.envelope.project_identity()
    }

    pub fn context_key(&self) -> &StackReviewAiContextKey {
        self.envelope.context_key()
    }

    pub fn context_revision(&self) -> &SharedString {
        self.envelope.context_revision()
    }

    pub fn projection_target(&self) -> Option<&SharedString> {
        self.envelope.projection_target()
    }

    pub fn markdown(&self) -> &Entity<Markdown> {
        &self.markdown
    }
}

pub struct StackReviewAiProjection {
    generation: StackReviewAiGeneration,
    session_id: Option<SharedString>,
    status: StackReviewAiStatus,
    assistant_turns: Vec<StackReviewAiTurn>,
}

impl StackReviewAiProjection {
    pub fn new(generation: StackReviewAiGeneration) -> Self {
        Self {
            generation,
            session_id: None,
            status: StackReviewAiStatus::Loading,
            assistant_turns: Vec::new(),
        }
    }

    pub fn generation(&self) -> StackReviewAiGeneration {
        self.generation
    }

    pub fn session_id(&self) -> Option<&SharedString> {
        self.session_id.as_ref()
    }

    pub fn status(&self) -> &StackReviewAiStatus {
        &self.status
    }

    pub fn assistant_turns(&self) -> &[StackReviewAiTurn] {
        &self.assistant_turns
    }

    pub fn begin_generation(
        &mut self,
        generation: StackReviewAiGeneration,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation <= self.generation {
            return false;
        }

        self.generation = generation;
        self.session_id = None;
        self.status = StackReviewAiStatus::Loading;
        self.assistant_turns.clear();
        cx.notify();
        true
    }

    pub fn apply_update(
        &mut self,
        update: StackReviewAiProjectionUpdate,
        cx: &mut Context<Self>,
    ) -> bool {
        if update.generation != self.generation {
            return false;
        }

        self.session_id = update.session_id;
        self.status = update.status;
        self.assistant_turns = update.assistant_turns;
        cx.notify();
        true
    }
}

#[derive(Clone)]
pub struct StackReviewAiProjectionUpdate {
    pub generation: StackReviewAiGeneration,
    pub session_id: Option<SharedString>,
    pub status: StackReviewAiStatus,
    pub assistant_turns: Vec<StackReviewAiTurn>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StackReviewAiContextKey {
    Review {
        storage_key: SharedString,
    },
    Comment {
        storage_key: SharedString,
        thread_key: SharedString,
    },
}

impl StackReviewAiContextKey {
    pub fn review(storage_key: impl Into<SharedString>) -> Self {
        Self::Review {
            storage_key: storage_key.into(),
        }
    }

    pub fn comment(
        storage_key: impl Into<SharedString>,
        thread_key: impl Into<SharedString>,
    ) -> Self {
        Self::Comment {
            storage_key: storage_key.into(),
            thread_key: thread_key.into(),
        }
    }

    pub fn storage_key(&self) -> &SharedString {
        match self {
            Self::Review { storage_key } | Self::Comment { storage_key, .. } => storage_key,
        }
    }

    pub fn stable_key(&self) -> SharedString {
        match self {
            Self::Review { storage_key } => {
                format!("review:{}:{storage_key}", storage_key.len()).into()
            }
            Self::Comment {
                storage_key,
                thread_key,
            } => format!(
                "comment:{}:{storage_key}:{}:{thread_key}",
                storage_key.len(),
                thread_key.len()
            )
            .into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackReviewAiResource {
    pub label: SharedString,
    pub uri: SharedString,
    pub text: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackReviewAiContext {
    pub key: StackReviewAiContextKey,
    pub context_revision: SharedString,
    pub title: SharedString,
    pub base_oid: SharedString,
    pub head_oid: SharedString,
    pub path: Option<SharedString>,
    pub side: Option<StackReviewCommentSide>,
    pub line_range: Option<Range<u32>>,
    pub selected_record_id: Option<SharedString>,
    pub resources: Arc<[StackReviewAiResource]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackReviewAiActivationRequest {
    pub context: StackReviewAiContext,
    pub persisted_session_id: Option<SharedString>,
    pub generation: StackReviewAiGeneration,
}

pub const STACK_REVIEW_TURN_ENVELOPE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackReviewTurnEnvelope {
    schema_version: u32,
    turn_id: SharedString,
    project_identity: SharedString,
    storage_key: SharedString,
    context_key: StackReviewAiContextKey,
    context_revision: SharedString,
    selected_record_id: Option<SharedString>,
    projection_target: Option<SharedString>,
}

impl StackReviewTurnEnvelope {
    pub fn new(
        turn_id: impl Into<SharedString>,
        project_identity: impl Into<SharedString>,
        context: &StackReviewAiContext,
        projection_target: Option<SharedString>,
    ) -> Self {
        Self {
            schema_version: STACK_REVIEW_TURN_ENVELOPE_SCHEMA_VERSION,
            turn_id: turn_id.into(),
            project_identity: project_identity.into(),
            storage_key: context.key.storage_key().clone(),
            context_key: context.key.clone(),
            context_revision: context.context_revision.clone(),
            selected_record_id: context.selected_record_id.clone(),
            projection_target,
        }
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn turn_id(&self) -> &SharedString {
        &self.turn_id
    }

    pub fn project_identity(&self) -> &SharedString {
        &self.project_identity
    }

    pub fn storage_key(&self) -> &SharedString {
        &self.storage_key
    }

    pub fn context_key(&self) -> &StackReviewAiContextKey {
        &self.context_key
    }

    pub fn context_revision(&self) -> &SharedString {
        &self.context_revision
    }

    pub fn selected_record_id(&self) -> Option<&SharedString> {
        self.selected_record_id.as_ref()
    }

    pub fn projection_target(&self) -> Option<&SharedString> {
        self.projection_target.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackReviewAiSubmitRequest {
    context: StackReviewAiContext,
    prompt: SharedString,
    envelope: StackReviewTurnEnvelope,
    generation: StackReviewAiGeneration,
}

impl StackReviewAiSubmitRequest {
    pub fn new(
        context: StackReviewAiContext,
        prompt: impl Into<SharedString>,
        turn_id: impl Into<SharedString>,
        project_identity: impl Into<SharedString>,
        projection_target: Option<SharedString>,
        generation: StackReviewAiGeneration,
    ) -> Self {
        let envelope =
            StackReviewTurnEnvelope::new(turn_id, project_identity, &context, projection_target);
        Self {
            context,
            prompt: prompt.into(),
            envelope,
            generation,
        }
    }

    pub fn context(&self) -> &StackReviewAiContext {
        &self.context
    }

    pub fn prompt(&self) -> &SharedString {
        &self.prompt
    }

    pub fn envelope(&self) -> &StackReviewTurnEnvelope {
        &self.envelope
    }

    pub fn generation(&self) -> StackReviewAiGeneration {
        self.generation
    }
}

pub trait StackReviewAiHost {
    fn activate_context(
        &self,
        request: StackReviewAiActivationRequest,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> gpui::Task<anyhow::Result<Entity<StackReviewAiProjection>>>;

    fn update_context(
        &self,
        context: StackReviewAiContext,
        generation: StackReviewAiGeneration,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> anyhow::Result<()>;

    fn reveal_and_focus_thread(
        &self,
        context_key: &StackReviewAiContextKey,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> anyhow::Result<()>;

    fn submit_local_prompt(
        &self,
        request: StackReviewAiSubmitRequest,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> gpui::Task<anyhow::Result<()>>;
}

struct StackReviewAiHostGlobal(Rc<dyn StackReviewAiHost>);

impl Global for StackReviewAiHostGlobal {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackReviewAiHostUnavailable;

impl fmt::Display for StackReviewAiHostUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Stack Review AI host is unavailable")
    }
}

impl std::error::Error for StackReviewAiHostUnavailable {}

pub fn set_stack_review_ai_host(host: Rc<dyn StackReviewAiHost>, cx: &mut App) {
    cx.set_global(StackReviewAiHostGlobal(host));
}

pub fn stack_review_ai_host(
    cx: &App,
) -> Result<Rc<dyn StackReviewAiHost>, StackReviewAiHostUnavailable> {
    cx.try_global::<StackReviewAiHostGlobal>()
        .map(|host| host.0.clone())
        .ok_or(StackReviewAiHostUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use gpui::AppContext as _;

    #[derive(Default)]
    struct TestHost {
        activations: std::cell::RefCell<Vec<StackReviewAiActivationRequest>>,
        context_updates: std::cell::RefCell<Vec<(StackReviewAiContext, StackReviewAiGeneration)>>,
        revealed_contexts: std::cell::RefCell<Vec<StackReviewAiContextKey>>,
        submissions: std::cell::RefCell<Vec<StackReviewAiSubmitRequest>>,
    }

    impl StackReviewAiHost for TestHost {
        fn activate_context(
            &self,
            request: StackReviewAiActivationRequest,
            _window: &mut gpui::Window,
            cx: &mut gpui::App,
        ) -> gpui::Task<anyhow::Result<gpui::Entity<StackReviewAiProjection>>> {
            let generation = request.generation;
            self.activations.borrow_mut().push(request);
            gpui::Task::ready(Ok(cx.new(|_| StackReviewAiProjection::new(generation))))
        }

        fn update_context(
            &self,
            context: StackReviewAiContext,
            generation: StackReviewAiGeneration,
            _window: &mut gpui::Window,
            _cx: &mut gpui::App,
        ) -> anyhow::Result<()> {
            self.context_updates
                .borrow_mut()
                .push((context, generation));
            Ok(())
        }

        fn reveal_and_focus_thread(
            &self,
            context_key: &StackReviewAiContextKey,
            _window: &mut gpui::Window,
            _cx: &mut gpui::App,
        ) -> anyhow::Result<()> {
            self.revealed_contexts
                .borrow_mut()
                .push(context_key.clone());
            Ok(())
        }

        fn submit_local_prompt(
            &self,
            request: StackReviewAiSubmitRequest,
            _window: &mut gpui::Window,
            _cx: &mut gpui::App,
        ) -> gpui::Task<anyhow::Result<()>> {
            self.submissions.borrow_mut().push(request);
            gpui::Task::ready(Ok(()))
        }
    }

    fn test_context() -> StackReviewAiContext {
        StackReviewAiContext {
            key: StackReviewAiContextKey::comment("base-head", "thread-a"),
            context_revision: "revision-1".into(),
            title: "Review · src/main.rs · Ada".into(),
            base_oid: "base".into(),
            head_oid: "head".into(),
            path: Some("src/main.rs".into()),
            side: Some(StackReviewCommentSide::Right),
            line_range: Some(3..8),
            selected_record_id: Some("comment-a".into()),
            resources: Vec::new().into(),
        }
    }

    #[gpui::test]
    fn reports_when_host_is_unavailable(cx: &mut gpui::TestAppContext) {
        let error = cx.read(|cx| match stack_review_ai_host(cx) {
            Ok(_) => panic!("expected Stack Review AI host to be unavailable"),
            Err(error) => error,
        });

        assert_eq!(error, StackReviewAiHostUnavailable);
        assert_eq!(error.to_string(), "Stack Review AI host is unavailable");
    }

    #[test]
    fn submit_envelope_is_derived_from_the_exact_context() {
        let context = test_context();
        let envelope =
            StackReviewTurnEnvelope::new("turn-1", "project-a", &context, Some("comment-a".into()));

        assert_eq!(
            envelope.schema_version(),
            STACK_REVIEW_TURN_ENVELOPE_SCHEMA_VERSION
        );
        assert_eq!(envelope.turn_id().as_ref(), "turn-1");
        assert_eq!(envelope.project_identity().as_ref(), "project-a");
        assert_eq!(envelope.storage_key(), context.key.storage_key());
        assert_eq!(envelope.context_key(), &context.key);
        assert_eq!(envelope.context_revision(), &context.context_revision);
        assert_eq!(
            envelope.selected_record_id(),
            context.selected_record_id.as_ref()
        );
        assert_eq!(
            envelope.projection_target().map(SharedString::as_ref),
            Some("comment-a")
        );
    }

    #[gpui::test]
    fn registered_host_preserves_dispatched_arguments(cx: &mut gpui::TestAppContext) {
        let host = Rc::new(TestHost::default());
        let generation = StackReviewAiGeneration::default()
            .next()
            .expect("initial generation is available");
        let context = test_context();
        let activation = StackReviewAiActivationRequest {
            context: context.clone(),
            persisted_session_id: Some("session-1".into()),
            generation,
        };
        let submission = StackReviewAiSubmitRequest::new(
            context.clone(),
            "Please review this comment.",
            "turn-1",
            "project-a",
            Some("comment-a".into()),
            generation,
        );

        let visual_context = cx.add_empty_window();
        visual_context.update(|window, cx| {
            set_stack_review_ai_host(host.clone(), cx);
            let registered_host = match stack_review_ai_host(cx) {
                Ok(host) => host,
                Err(error) => panic!("failed to access registered host: {error}"),
            };

            let projection = match registered_host
                .activate_context(activation.clone(), window, cx)
                .now_or_never()
            {
                Some(Ok(projection)) => projection,
                Some(Err(error)) => panic!("activation failed: {error}"),
                None => panic!("ready activation task did not complete"),
            };
            assert_eq!(projection.read(cx).generation(), generation);
            assert!(
                registered_host
                    .update_context(context.clone(), generation, window, cx)
                    .is_ok()
            );
            assert!(
                registered_host
                    .reveal_and_focus_thread(&context.key, window, cx)
                    .is_ok()
            );
            assert!(matches!(
                registered_host
                    .submit_local_prompt(submission.clone(), window, cx)
                    .now_or_never(),
                Some(Ok(()))
            ));
        });

        assert_eq!(host.activations.borrow().as_slice(), &[activation]);
        assert_eq!(
            host.context_updates.borrow().as_slice(),
            &[(context.clone(), generation)]
        );
        assert_eq!(
            host.revealed_contexts.borrow().as_slice(),
            std::slice::from_ref(&context.key)
        );
        assert_eq!(host.submissions.borrow().as_slice(), &[submission]);
    }

    #[test]
    fn context_keys_are_deterministic_within_snapshot_partitions() {
        let snapshot = git::stack_review::stack_review_storage_key("base-a", "head-a");
        let other_snapshot = git::stack_review::stack_review_storage_key("base-b", "head-b");
        let thread = r#"{"kind":"normal","rootRecordId":"root-a"}"#;

        let review = StackReviewAiContextKey::review(snapshot.clone());
        let same_review = StackReviewAiContextKey::review(snapshot.clone());
        let comment = StackReviewAiContextKey::comment(snapshot.clone(), thread);

        assert_eq!(review, same_review);
        assert_eq!(
            review.stable_key().as_ref(),
            format!("review:{}:{snapshot}", snapshot.len())
        );
        assert_eq!(
            comment.stable_key().as_ref(),
            format!(
                "comment:{}:{snapshot}:{}:{thread}",
                snapshot.len(),
                thread.len()
            )
        );
        assert_eq!(review.storage_key().as_ref(), snapshot);
        assert_ne!(review, comment);
        assert_ne!(
            comment,
            StackReviewAiContextKey::comment(other_snapshot, thread)
        );
        assert_ne!(
            comment,
            StackReviewAiContextKey::comment(snapshot, "another-thread")
        );
    }

    #[test]
    fn stable_keys_are_unambiguous_for_arbitrary_components() {
        assert_ne!(
            StackReviewAiContextKey::comment("a:b", "c").stable_key(),
            StackReviewAiContextKey::comment("a", "b:c").stable_key()
        );
    }

    #[gpui::test]
    fn projection_accepts_current_generation_update(cx: &mut gpui::TestAppContext) {
        let generation = StackReviewAiGeneration::default()
            .next()
            .expect("initial generation is available");
        let projection = cx.update(|cx| cx.new(|_| StackReviewAiProjection::new(generation)));
        let markdown = cx.update(|cx| cx.new(|cx| Markdown::new_text("assistant".into(), cx)));
        let context = test_context();
        let envelope =
            StackReviewTurnEnvelope::new("turn-1", "project-a", &context, Some("comment-a".into()));
        let turn = StackReviewAiTurn::from_envelope(envelope, markdown.clone());

        let applied = projection.update(cx, |projection, cx| {
            projection.apply_update(
                StackReviewAiProjectionUpdate {
                    generation,
                    session_id: Some("session-1".into()),
                    status: StackReviewAiStatus::Ready,
                    assistant_turns: vec![turn.clone()],
                },
                cx,
            )
        });

        assert!(applied);
        cx.read(|cx| {
            let projection = projection.read(cx);
            assert_eq!(projection.generation(), generation);
            assert_eq!(
                projection.session_id().map(SharedString::as_ref),
                Some("session-1")
            );
            assert_eq!(projection.status(), &StackReviewAiStatus::Ready);
            assert_eq!(projection.assistant_turns().len(), 1);
            let projected_turn = &projection.assistant_turns()[0];
            assert_eq!(projected_turn.turn_id().as_ref(), "turn-1");
            assert_eq!(projected_turn.project_identity().as_ref(), "project-a");
            assert_eq!(projected_turn.context_revision().as_ref(), "revision-1");
            assert_eq!(projected_turn.context_key(), turn.context_key());
            assert_eq!(
                projected_turn.projection_target().map(SharedString::as_ref),
                Some("comment-a")
            );
            assert_eq!(projected_turn.markdown().entity_id(), markdown.entity_id());
        });
    }

    #[test]
    fn generation_exhaustion_is_explicit() {
        assert_eq!(StackReviewAiGeneration(u64::MAX).next(), None);
    }

    #[gpui::test]
    fn projection_rejects_stale_generation_after_advancing(cx: &mut gpui::TestAppContext) {
        let stale_generation = StackReviewAiGeneration::default()
            .next()
            .expect("initial generation is available");
        let current_generation = stale_generation
            .next()
            .expect("next generation is available");
        let projection = cx.update(|cx| cx.new(|_| StackReviewAiProjection::new(stale_generation)));

        projection.update(cx, |projection, cx| {
            assert!(projection.begin_generation(current_generation, cx));
        });
        let applied = projection.update(cx, |projection, cx| {
            projection.apply_update(
                StackReviewAiProjectionUpdate {
                    generation: stale_generation,
                    session_id: Some("stale-session".into()),
                    status: StackReviewAiStatus::Ready,
                    assistant_turns: Vec::new(),
                },
                cx,
            )
        });

        assert!(!applied);
        cx.read(|cx| {
            let projection = projection.read(cx);
            assert_eq!(projection.generation(), current_generation);
            assert_eq!(projection.session_id(), None);
            assert_eq!(projection.status(), &StackReviewAiStatus::Loading);
            assert!(projection.assistant_turns().is_empty());
        });
    }

    #[test]
    fn context_preserves_snapshot_side_range_and_shared_resources() {
        let resource_text: std::sync::Arc<str> = "immutable code".into();
        let resources: std::sync::Arc<[StackReviewAiResource]> = vec![StackReviewAiResource {
            label: "src/main.rs (RIGHT 4-8)".into(),
            uri: "zed-stack-review://snapshot/src/main.rs?side=right".into(),
            text: resource_text.clone(),
        }]
        .into();
        let context = StackReviewAiContext {
            key: StackReviewAiContextKey::comment("base-head", "thread-a"),
            context_revision: "revision-1".into(),
            title: "Review · src/main.rs · Ada".into(),
            base_oid: "base".into(),
            head_oid: "head".into(),
            path: Some("src/main.rs".into()),
            side: Some(git::stack_review::StackReviewCommentSide::Right),
            line_range: Some(3..8),
            selected_record_id: Some("comment-a".into()),
            resources: resources.clone(),
        };

        assert_eq!(context.key.storage_key().as_ref(), "base-head");
        assert_eq!(
            context.side,
            Some(git::stack_review::StackReviewCommentSide::Right)
        );
        assert_eq!(context.line_range, Some(3..8));
        assert!(std::sync::Arc::ptr_eq(
            &context.resources[0].text,
            &resource_text
        ));
        assert!(std::sync::Arc::ptr_eq(&context.resources, &resources));
    }
}
