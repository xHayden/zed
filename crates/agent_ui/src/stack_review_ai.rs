use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};

use acp_thread::{AcpThreadEvent, AgentThreadEntry, AssistantMessageChunk, ThreadStatus};
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result};
use git_ui_core::stack_review_ai::{
    STACK_REVIEW_TURN_ENVELOPE_URI, StackReviewAiActivationRequest, StackReviewAiContext,
    StackReviewAiContextKey, StackReviewAiGeneration, StackReviewAiHost, StackReviewAiProjection,
    StackReviewAiProjectionUpdate, StackReviewAiStatus, StackReviewAiSubmitRequest,
    StackReviewAiTurn, StackReviewTurnEnvelope, set_stack_review_ai_host,
};
use gpui::{App, AppContext as _, Context, Entity, EntityId, Task, WeakEntity, Window};
use uuid::Uuid;
use workspace::Workspace;

use crate::{Agent, AgentInitialContent, AgentPanel, ConversationView, StateChange};

pub(crate) struct ZedStackReviewAiHost;

pub(crate) fn stack_review_turn_envelope(
    chunks: &[acp::ContentBlock],
    context_key: &StackReviewAiContextKey,
    project_identity: &gpui::SharedString,
) -> Option<StackReviewTurnEnvelope> {
    let mut envelopes = chunks.iter().filter_map(|chunk| match chunk {
        acp::ContentBlock::Text(text) => {
            StackReviewTurnEnvelope::from_metadata_text(&text.text).ok()
        }
        acp::ContentBlock::Resource(resource) => match &resource.resource {
            acp::EmbeddedResourceResource::TextResourceContents(resource)
                if resource.uri == STACK_REVIEW_TURN_ENVELOPE_URI =>
            {
                StackReviewTurnEnvelope::from_metadata_text(&resource.text).ok()
            }
            _ => None,
        },
        _ => None,
    });
    envelopes.next().filter(|envelope| {
        envelopes.next().is_none()
            && envelope.context_key() == context_key
            && envelope.project_identity() == project_identity
    })
}

fn stack_review_status_for_stop(reason: acp::StopReason) -> StackReviewAiStatus {
    match reason {
        acp::StopReason::EndTurn => StackReviewAiStatus::Ready,
        acp::StopReason::Cancelled => StackReviewAiStatus::Canceled,
        acp::StopReason::MaxTokens => {
            StackReviewAiStatus::Failed("Agent reached its token limit".into())
        }
        acp::StopReason::MaxTurnRequests => {
            StackReviewAiStatus::Failed("Agent reached its turn request limit".into())
        }
        acp::StopReason::Refusal => StackReviewAiStatus::Failed("Agent refused the prompt".into()),
        _ => StackReviewAiStatus::Failed("Agent stopped for an unsupported reason".into()),
    }
}

fn append_stack_review_context_with_envelope(
    contents: &mut Vec<acp::ContentBlock>,
    context: &StackReviewAiContext,
    envelope: &StackReviewTurnEnvelope,
) -> Result<()> {
    let origin = ZedStackReviewAiHost::thread_origin(context)?;
    anyhow::ensure!(
        envelope.project_identity() == &origin.project_identity,
        "Stack Review Agent envelope project does not match its context"
    );
    for resource in context.resources.iter() {
        let citation = resource.citation();
        anyhow::ensure!(
            citation.project_identity() == &origin.project_identity
                && citation.storage_key() == &origin.storage_key
                && citation.base_oid() == &origin.base_oid
                && citation.head_oid() == &origin.head_oid,
            "Stack Review Agent resource does not match its immutable context"
        );
        contents.push(acp::ContentBlock::Resource(acp::EmbeddedResource::new(
            acp::EmbeddedResourceResource::TextResourceContents(acp::TextResourceContents::new(
                resource.text().to_string(),
                resource.uri(),
            )),
        )));
    }
    contents.push(acp::ContentBlock::Resource(acp::EmbeddedResource::new(
        acp::EmbeddedResourceResource::TextResourceContents(acp::TextResourceContents::new(
            envelope.to_metadata_text()?,
            STACK_REVIEW_TURN_ENVELOPE_URI,
        )),
    )));
    Ok(())
}

pub(crate) fn is_stack_review_context_block(block: &acp::ContentBlock) -> bool {
    if let acp::ContentBlock::Text(text) = block {
        return StackReviewTurnEnvelope::from_metadata_text(&text.text).is_ok();
    }
    let acp::ContentBlock::Resource(resource) = block else {
        return false;
    };
    let acp::EmbeddedResourceResource::TextResourceContents(resource) = &resource.resource else {
        return false;
    };
    resource.uri == STACK_REVIEW_TURN_ENVELOPE_URI
        || resource.uri.starts_with("zed:///agent/stack-review?")
}

pub(crate) fn renew_stack_review_turn_envelope(contents: &mut [acp::ContentBlock]) -> Result<()> {
    let mut envelope_count = 0;
    for block in contents {
        let envelope_text = match block {
            acp::ContentBlock::Resource(resource) => match &mut resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(resource)
                    if resource.uri == STACK_REVIEW_TURN_ENVELOPE_URI =>
                {
                    Some(&mut resource.text)
                }
                _ => None,
            },
            acp::ContentBlock::Text(text)
                if StackReviewTurnEnvelope::from_metadata_text(&text.text).is_ok() =>
            {
                Some(&mut text.text)
            }
            _ => None,
        };
        let Some(envelope_text) = envelope_text else {
            continue;
        };
        envelope_count += 1;
        let envelope = StackReviewTurnEnvelope::from_metadata_text(envelope_text)?;
        *envelope_text = envelope
            .with_turn_id(Uuid::new_v4().to_string())?
            .to_metadata_text()?;
    }
    anyhow::ensure!(
        envelope_count <= 1,
        "Stack Review Agent turn contains duplicate envelopes"
    );
    Ok(())
}

impl ZedStackReviewAiHost {
    fn thread_origin(context: &StackReviewAiContext) -> Result<agent::StackReviewThreadOrigin> {
        let citation = context
            .resources
            .first()
            .map(|resource| resource.citation())
            .context("Stack Review AI context has no immutable citation")?;
        anyhow::ensure!(
            citation.storage_key() == context.key.storage_key(),
            "Stack Review AI citation storage key does not match its context"
        );
        anyhow::ensure!(
            citation.base_oid() == &context.base_oid && citation.head_oid() == &context.head_oid,
            "Stack Review AI citation snapshot does not match its context"
        );
        Ok(agent::StackReviewThreadOrigin {
            project_identity: citation.project_identity().clone(),
            storage_key: context.key.storage_key().clone(),
            context_key: context.key.stable_key(),
            base_oid: context.base_oid.clone(),
            head_oid: context.head_oid.clone(),
        })
    }

    fn sync_projection(
        projection: &mut StackReviewAiProjection,
        thread: &Entity<acp_thread::AcpThread>,
        context_key: &StackReviewAiContextKey,
        project_identity: &gpui::SharedString,
        generation: StackReviewAiGeneration,
        cx: &mut Context<StackReviewAiProjection>,
    ) -> HashSet<EntityId> {
        let thread = thread.read(cx);
        let session_id = thread.session_id().to_string().into();
        let status = match thread.status() {
            ThreadStatus::Idle => StackReviewAiStatus::Ready,
            ThreadStatus::Generating => StackReviewAiStatus::Generating,
        };
        let mut envelope = None;
        let mut assistant_turns = Vec::new();
        let mut observed_markdown_ids = HashSet::default();
        for entry in thread.entries() {
            match entry {
                AgentThreadEntry::UserMessage(message) => {
                    envelope =
                        stack_review_turn_envelope(&message.chunks, context_key, project_identity);
                }
                AgentThreadEntry::AssistantMessage(message) => {
                    for chunk in &message.chunks {
                        let AssistantMessageChunk::Message { block, .. } = chunk else {
                            continue;
                        };
                        let Some(markdown) = block.markdown().cloned() else {
                            continue;
                        };
                        observed_markdown_ids.insert(markdown.entity_id());
                        if let Some(envelope) = envelope.as_ref() {
                            assistant_turns
                                .push(StackReviewAiTurn::from_envelope(envelope.clone(), markdown));
                        }
                    }
                }
                AgentThreadEntry::ToolCall(_)
                | AgentThreadEntry::Elicitation(_)
                | AgentThreadEntry::CompletedPlan(_)
                | AgentThreadEntry::ContextCompaction(_) => {}
            }
        }
        projection.apply_update(
            StackReviewAiProjectionUpdate {
                generation,
                session_id: Some(session_id),
                status,
                assistant_turns,
            },
            cx,
        );
        observed_markdown_ids
    }

    fn entry_has_new_markdown(
        observed_markdown_ids: &RefCell<HashSet<EntityId>>,
        thread: &Entity<acp_thread::AcpThread>,
        entry_index: usize,
        cx: &App,
    ) -> bool {
        let markdown_ids = thread
            .read(cx)
            .entries()
            .get(entry_index)
            .and_then(|entry| match entry {
                AgentThreadEntry::AssistantMessage(message) => Some(message),
                _ => None,
            })
            .into_iter()
            .flat_map(|message| &message.chunks)
            .filter_map(|chunk| match chunk {
                AssistantMessageChunk::Message { block, .. } => {
                    block.markdown().map(|markdown| markdown.entity_id())
                }
                AssistantMessageChunk::Thought { .. } => None,
            })
            .collect::<Vec<_>>();
        Self::observe_markdown_ids(observed_markdown_ids, markdown_ids)
    }

    fn observe_markdown_ids(
        observed_markdown_ids: &RefCell<HashSet<EntityId>>,
        markdown_ids: impl IntoIterator<Item = EntityId>,
    ) -> bool {
        let mut observed_markdown_ids = observed_markdown_ids.borrow_mut();
        markdown_ids
            .into_iter()
            .any(|markdown_id| observed_markdown_ids.insert(markdown_id))
    }

    fn attach_thread_observer(
        projection: &mut StackReviewAiProjection,
        conversation_view: &Entity<ConversationView>,
        observed_thread: &Rc<Cell<Option<EntityId>>>,
        observed_markdown_ids: &Rc<RefCell<HashSet<EntityId>>>,
        context_key: &StackReviewAiContextKey,
        project_identity: &gpui::SharedString,
        generation: StackReviewAiGeneration,
        cx: &mut Context<StackReviewAiProjection>,
    ) {
        let conversation = conversation_view.read(cx);
        let Some(thread_view) = conversation.root_thread_view() else {
            if let Some(error) = conversation.load_error() {
                projection.apply_update(
                    StackReviewAiProjectionUpdate {
                        generation,
                        session_id: None,
                        status: StackReviewAiStatus::Failed(error),
                        assistant_turns: Vec::new(),
                    },
                    cx,
                );
            } else if conversation.as_connected().is_some() {
                projection.apply_update(
                    StackReviewAiProjectionUpdate {
                        generation,
                        session_id: None,
                        status: StackReviewAiStatus::Failed(
                            "Agent authentication is required; open Agent Panel to sign in".into(),
                        ),
                        assistant_turns: Vec::new(),
                    },
                    cx,
                );
            }
            return;
        };
        let thread = thread_view.read(cx).thread.clone();
        let thread_id = thread.entity_id();
        let markdown_ids = Self::sync_projection(
            projection,
            &thread,
            context_key,
            project_identity,
            generation,
            cx,
        );
        *observed_markdown_ids.borrow_mut() = markdown_ids;
        if let Some(status) = thread_view.read(cx).stack_review_error_status() {
            projection.apply_status(generation, status, cx);
        }
        if observed_thread.replace(Some(thread_id)) == Some(thread_id) {
            return;
        }
        let observed_thread = observed_thread.clone();
        let observed_markdown_ids = observed_markdown_ids.clone();
        let context_key = context_key.clone();
        let project_identity = project_identity.clone();
        let subscription = cx.subscribe(&thread, move |projection, thread, event, cx| {
            if observed_thread.get() != Some(thread.entity_id()) {
                return;
            }
            match event {
                AcpThreadEvent::NewEntry | AcpThreadEvent::EntriesRemoved(_) => {
                    let markdown_ids = Self::sync_projection(
                        projection,
                        &thread,
                        &context_key,
                        &project_identity,
                        generation,
                        cx,
                    );
                    *observed_markdown_ids.borrow_mut() = markdown_ids;
                }
                AcpThreadEvent::EntryUpdated(entry_index) => {
                    if Self::entry_has_new_markdown(
                        &observed_markdown_ids,
                        &thread,
                        *entry_index,
                        cx,
                    ) {
                        let markdown_ids = Self::sync_projection(
                            projection,
                            &thread,
                            &context_key,
                            &project_identity,
                            generation,
                            cx,
                        );
                        *observed_markdown_ids.borrow_mut() = markdown_ids;
                    }
                }
                AcpThreadEvent::StatusChanged => {
                    let status = match thread.read(cx).status() {
                        ThreadStatus::Idle => StackReviewAiStatus::Ready,
                        ThreadStatus::Generating => StackReviewAiStatus::Generating,
                    };
                    projection.apply_status(generation, status, cx);
                }
                AcpThreadEvent::Stopped(reason) => {
                    projection.apply_status(generation, stack_review_status_for_stop(*reason), cx);
                }
                AcpThreadEvent::Error => {
                    let status = thread_view
                        .read(cx)
                        .stack_review_error_status()
                        .unwrap_or_else(|| StackReviewAiStatus::Failed("Agent turn failed".into()));
                    projection.apply_status(generation, status, cx);
                }
                AcpThreadEvent::LoadError(error) => {
                    projection.apply_status(
                        generation,
                        StackReviewAiStatus::Failed(error.to_string().into()),
                        cx,
                    );
                }
                AcpThreadEvent::Refusal => {
                    projection.apply_status(
                        generation,
                        StackReviewAiStatus::Failed("Agent refused the prompt".into()),
                        cx,
                    );
                }
                AcpThreadEvent::PromptUpdated
                | AcpThreadEvent::TitleUpdated
                | AcpThreadEvent::TokenUsageUpdated
                | AcpThreadEvent::ToolAuthorizationRequested(_)
                | AcpThreadEvent::ToolAuthorizationReceived(_)
                | AcpThreadEvent::ElicitationRequested(_)
                | AcpThreadEvent::ElicitationResponded(_)
                | AcpThreadEvent::Retry(_)
                | AcpThreadEvent::SubagentSpawned(_)
                | AcpThreadEvent::PromptCapabilitiesUpdated
                | AcpThreadEvent::AvailableCommandsUpdated(_)
                | AcpThreadEvent::ModeUpdated(_)
                | AcpThreadEvent::ConfigOptionsUpdated(_)
                | AcpThreadEvent::WorkingDirectoriesUpdated => {}
            }
        });
        projection.retain_subscription(subscription);
    }

    fn content_blocks(request: &StackReviewAiSubmitRequest) -> Result<Vec<acp::ContentBlock>> {
        anyhow::ensure!(
            !request.prompt().trim().is_empty(),
            "Stack Review Agent prompt is empty"
        );
        let mut blocks = Vec::with_capacity(request.context().resources.len() + 2);
        blocks.push(acp::ContentBlock::Text(acp::TextContent::new(
            request.prompt().to_string(),
        )));
        append_stack_review_context_with_envelope(
            &mut blocks,
            request.context(),
            request.envelope(),
        )?;
        Ok(blocks)
    }

    fn composer_initial_content(context: &StackReviewAiContext) -> Result<AgentInitialContent> {
        let project_identity = context
            .resources
            .first()
            .map(|resource| resource.citation().project_identity().clone())
            .context("Stack Review AI context has no immutable citation")?;
        let envelope = StackReviewTurnEnvelope::new(
            Uuid::new_v4().to_string(),
            project_identity,
            context,
            context.selected_record_id.clone(),
        );
        let mut blocks = Vec::with_capacity(context.resources.len() + 1);
        append_stack_review_context_with_envelope(&mut blocks, context, &envelope)?;
        Ok(AgentInitialContent::ContentBlock {
            blocks,
            auto_submit: false,
        })
    }
}

impl StackReviewAiHost for ZedStackReviewAiHost {
    fn activate_context(
        &self,
        request: StackReviewAiActivationRequest,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<StackReviewAiProjection>>> {
        let origin = match Self::thread_origin(&request.context) {
            Ok(origin) => origin,
            Err(error) => return Task::ready(Err(error)),
        };
        let initial_content = if request.prepare_composer {
            match Self::composer_initial_content(&request.context) {
                Ok(initial_content) => Some(initial_content),
                Err(error) => return Task::ready(Err(error)),
            }
        } else {
            None
        };
        let projection = cx.new(|_| StackReviewAiProjection::new(request.generation));
        let projection_context_key = request.context.key.clone();
        let projection_project_identity = origin.project_identity.clone();
        window.spawn(cx, async move |cx| {
            let conversation_view = workspace.update_in(
                cx,
                |workspace, window, cx| -> Result<Entity<ConversationView>> {
                    let panel = workspace
                        .panel::<AgentPanel>(cx)
                        .context("Agent Panel is unavailable in this workspace")?;
                    let selected_agent = panel.read(cx).selected_agent(cx);
                    if let Some(session_id) = &request.persisted_session_id {
                        let session_id = agent_client_protocol::schema::v1::SessionId::new(
                            session_id.to_string(),
                        );
                        panel.update(cx, |panel, cx| {
                            panel.resume_stack_review_thread(
                                Agent::NativeAgent,
                                session_id,
                                origin,
                                request.context.title.clone(),
                                initial_content,
                                window,
                                cx,
                            )
                        })
                    } else {
                        let thread_id = panel.update(cx, |panel, cx| {
                            panel.create_stack_review_thread(
                                selected_agent,
                                origin,
                                request.context.title.clone(),
                                initial_content,
                                window,
                                cx,
                            )
                        });
                        panel
                            .read(cx)
                            .conversation_view_for_id(&thread_id, cx)
                            .cloned()
                            .context("Stack Review Agent conversation was not retained")
                    }
                },
            )??;
            projection.update(cx, |projection, cx| {
                projection.set_session_owner(conversation_view.read(cx).thread_id.to_key_string());
                if conversation_view.read(cx).agent_key() == &Agent::NativeAgent {
                    projection.allow_durable_binding();
                }
                let generation = request.generation;
                let observed_thread = Rc::new(Cell::new(None));
                let observed_markdown_ids = Rc::new(RefCell::new(HashSet::default()));
                let context_key = projection_context_key.clone();
                let project_identity = projection_project_identity.clone();
                let observer_thread = observed_thread.clone();
                let observer_markdown_ids = observed_markdown_ids.clone();
                let subscription = cx.subscribe(
                    &conversation_view,
                    move |projection, conversation_view, _: &StateChange, cx| {
                        Self::attach_thread_observer(
                            projection,
                            &conversation_view,
                            &observer_thread,
                            &observer_markdown_ids,
                            &context_key,
                            &project_identity,
                            generation,
                            cx,
                        );
                    },
                );
                projection.retain_subscription(subscription);
                Self::attach_thread_observer(
                    projection,
                    &conversation_view,
                    &observed_thread,
                    &observed_markdown_ids,
                    &projection_context_key,
                    &projection_project_identity,
                    generation,
                    cx,
                );
            });
            Ok(projection)
        })
    }

    fn prepare_composer(
        &self,
        context: StackReviewAiContext,
        session_id: &gpui::SharedString,
        session_owner: &gpui::SharedString,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let origin = Self::thread_origin(&context)?;
        let initial_content = Self::composer_initial_content(&context)?;
        let session_id = acp::SessionId::new(session_id.to_string());
        let workspace = workspace
            .upgrade()
            .context("Stack Review workspace is unavailable")?;
        let panel = workspace
            .read(cx)
            .panel::<AgentPanel>(cx)
            .context("Agent Panel is unavailable in this workspace")?;
        panel.update(cx, |panel, cx| {
            panel.prepare_stack_review_composer(
                session_owner,
                &session_id,
                &origin,
                &initial_content,
                window,
                cx,
            )
        })
    }

    fn reveal_and_focus_thread(
        &self,
        session_id: &gpui::SharedString,
        session_owner: &gpui::SharedString,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let session_id = agent_client_protocol::schema::v1::SessionId::new(session_id.to_string());
        workspace.update(cx, |workspace, cx| {
            workspace.reveal_panel::<AgentPanel>(window, cx);
            let panel = workspace
                .panel::<AgentPanel>(cx)
                .context("Agent Panel is unavailable in this workspace")?;
            let thread_id = panel
                .read(cx)
                .thread_id_for_owned_session(session_owner, &session_id, cx)
                .context("Stack Review Agent session is unavailable in this workspace")?;
            panel.update(cx, |panel, cx| {
                if panel.active_thread_id(cx) != Some(thread_id) {
                    panel.activate_retained_thread(thread_id, true, window, cx);
                }
            });
            workspace.focus_panel::<AgentPanel>(window, cx);
            Ok::<(), anyhow::Error>(())
        })??;
        Ok(())
    }

    fn submit_local_prompt(
        &self,
        request: StackReviewAiSubmitRequest,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let contents = match Self::content_blocks(&request) {
            Ok(contents) => contents,
            Err(error) => return Task::ready(Err(error)),
        };
        let expected_origin = match Self::thread_origin(request.context()) {
            Ok(origin) => origin,
            Err(error) => return Task::ready(Err(error)),
        };

        let session_id =
            agent_client_protocol::schema::v1::SessionId::new(request.session_id().to_string());
        let result = (|| -> Result<()> {
            let workspace = workspace
                .upgrade()
                .context("Stack Review workspace is unavailable")?;
            let panel = workspace
                .read(cx)
                .panel::<AgentPanel>(cx)
                .context("Agent Panel is unavailable in this workspace")?;
            let thread_id = panel
                .read(cx)
                .thread_id_for_owned_session(request.session_owner(), &session_id, cx)
                .context("Stack Review Agent session is unavailable in this workspace")?;
            let conversation_view = panel
                .read(cx)
                .conversation_view_for_id(&thread_id, cx)
                .cloned()
                .context("Stack Review Agent conversation is unavailable")?;
            anyhow::ensure!(
                conversation_view.read(cx).thread_id.to_key_string()
                    == request.session_owner().as_ref(),
                "Stack Review Agent session owner does not match"
            );
            anyhow::ensure!(
                conversation_view.read(cx).stack_review_origin() == Some(&expected_origin),
                "Stack Review Agent origin does not match the submitted context"
            );
            conversation_view
                .read(cx)
                .validate_stack_review_submission(cx)?;
            let thread_view = conversation_view
                .read(cx)
                .root_thread_view()
                .context("Stack Review Agent thread is not ready")?;
            thread_view.update(cx, |thread_view, cx| {
                thread_view.submit_content_blocks(contents, window, cx)
            })
        })();
        Task::ready(result)
    }
}

pub(crate) fn init(cx: &mut App) {
    set_stack_review_ai_host(Rc::new(ZedStackReviewAiHost), cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> StackReviewAiContext {
        let citation = git_ui_core::stack_review_ai::StackReviewCitationNavigationRequest::try_new(
            "base-head",
            "project-a",
            "base",
            "head",
            Some("src/main.rs".into()),
            git::stack_review::StackReviewCommentSide::Right,
            Some(3..=8),
            Some("comment-a".into()),
            Some("comment-a".into()),
        )
        .unwrap();
        StackReviewAiContext {
            key: StackReviewAiContextKey::comment("base-head", "thread-a"),
            context_revision: "revision-1".into(),
            title: "Review · src/main.rs".into(),
            base_oid: "base".into(),
            head_oid: "head".into(),
            path: Some("src/main.rs".into()),
            side: Some(git::stack_review::StackReviewCommentSide::Right),
            line_range: Some(3..8),
            selected_record_id: Some("comment-a".into()),
            resources: vec![git_ui_core::stack_review_ai::StackReviewAiResource::new(
                "src/main.rs",
                citation,
                "immutable code",
            )]
            .into(),
        }
    }

    #[test]
    fn stack_review_composer_context_is_canonical_and_never_auto_submits() {
        let context = test_context();
        let AgentInitialContent::ContentBlock {
            blocks,
            auto_submit,
        } = ZedStackReviewAiHost::composer_initial_content(&context).unwrap()
        else {
            panic!("expected content-block initial content");
        };
        assert!(!auto_submit);
        assert_eq!(blocks.len(), context.resources.len() + 1);
        let envelope = stack_review_turn_envelope(&blocks, &context.key, &"project-a".into())
            .expect("exactly one canonical envelope");
        assert_eq!(
            envelope.projection_target(),
            context.selected_record_id.as_ref()
        );
    }

    #[test]
    fn managed_stack_review_context_renews_turn_identity_per_send() {
        let context = test_context();
        let AgentInitialContent::ContentBlock { mut blocks, .. } =
            ZedStackReviewAiHost::composer_initial_content(&context).unwrap()
        else {
            panic!("expected content-block initial content");
        };
        let original = stack_review_turn_envelope(&blocks, &context.key, &"project-a".into())
            .expect("original envelope");

        renew_stack_review_turn_envelope(&mut blocks).expect("renew first turn");
        let first = stack_review_turn_envelope(&blocks, &context.key, &"project-a".into())
            .expect("first renewed envelope");
        renew_stack_review_turn_envelope(&mut blocks).expect("renew second turn");
        let second = stack_review_turn_envelope(&blocks, &context.key, &"project-a".into())
            .expect("second renewed envelope");

        assert_ne!(original.turn_id(), first.turn_id());
        assert_ne!(first.turn_id(), second.turn_id());
        assert_eq!(first.context_key(), second.context_key());
        assert_eq!(first.projection_target(), second.projection_target());
    }

    #[gpui::test]
    fn stack_review_markdown_identity_tracking_is_incremental(cx: &mut gpui::TestAppContext) {
        let first = cx.new(|_| ()).entity_id();
        let second = cx.new(|_| ()).entity_id();
        let observed = RefCell::new(HashSet::default());

        assert!(ZedStackReviewAiHost::observe_markdown_ids(
            &observed,
            [first]
        ));
        assert!(!ZedStackReviewAiHost::observe_markdown_ids(
            &observed,
            [first]
        ));
        assert!(ZedStackReviewAiHost::observe_markdown_ids(
            &observed,
            [first, second]
        ));
        assert_eq!(observed.borrow().len(), 2);
    }

    #[test]
    fn stack_review_stop_reasons_preserve_failure_semantics() {
        assert_eq!(
            stack_review_status_for_stop(acp::StopReason::EndTurn),
            StackReviewAiStatus::Ready
        );
        assert_eq!(
            stack_review_status_for_stop(acp::StopReason::Cancelled),
            StackReviewAiStatus::Canceled
        );
        assert!(matches!(
            stack_review_status_for_stop(acp::StopReason::MaxTokens),
            StackReviewAiStatus::Failed(_)
        ));
        assert!(matches!(
            stack_review_status_for_stop(acp::StopReason::MaxTurnRequests),
            StackReviewAiStatus::Failed(_)
        ));
        assert!(matches!(
            stack_review_status_for_stop(acp::StopReason::Refusal),
            StackReviewAiStatus::Failed(_)
        ));
    }
}
