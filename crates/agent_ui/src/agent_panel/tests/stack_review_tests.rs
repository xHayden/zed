use super::*;

#[gpui::test]
async fn test_create_stack_review_thread_uses_one_read_only_native_session(
    cx: &mut TestAppContext,
) {
    let (panel, mut cx) = setup_panel(cx).await;
    let origin = agent::StackReviewThreadOrigin {
        project_identity: "project-identity".into(),
        storage_key: "storage-key".into(),
        context_key: "comment:root-id".into(),
        base_oid: "1111111111111111111111111111111111111111".into(),
        head_oid: "2222222222222222222222222222222222222222".into(),
    };

    let thread_id = panel.update_in(&mut cx, |panel, window, cx| {
        panel.create_stack_review_thread(
            Agent::NativeAgent,
            origin.clone(),
            "Review thread".into(),
            None,
            window,
            cx,
        )
    });
    cx.run_until_parked();

    let conversation_view = panel
        .read_with(&cx, |panel, _cx| {
            panel.retained_threads.get(&thread_id).cloned()
        })
        .expect("retained Stack Review conversation");
    let acp_thread = conversation_view
        .read_with(&cx, |view, cx| view.root_thread(cx))
        .expect("Stack Review ACP thread");
    let native_thread = conversation_view
        .read_with(&cx, |view, cx| view.as_native_thread(cx))
        .expect("Stack Review native thread");

    assert_eq!(
        acp_thread.read_with(&cx, |thread, _cx| thread.session_id().clone()),
        native_thread.read_with(&cx, |thread, _cx| thread.id().clone())
    );
    native_thread.read_with(&cx, |thread, _cx| {
        assert_eq!(
            thread.execution_policy(),
            acp_thread::ThreadExecutionPolicy::ReadOnly
        );
        assert_eq!(thread.stack_review_origin(), Some(&origin));
    });
}

#[gpui::test]
async fn test_resume_stack_review_thread_rejects_foreign_origin_before_thread_view(
    cx: &mut TestAppContext,
) {
    let (panel, mut cx) = setup_panel(cx).await;
    let session_id = acp::SessionId::new("persisted-stack-review-thread");
    let persisted_origin = agent::StackReviewThreadOrigin {
        project_identity: "project-identity".into(),
        storage_key: "storage-key".into(),
        context_key: "comment:root-id".into(),
        base_oid: "1111111111111111111111111111111111111111".into(),
        head_oid: "2222222222222222222222222222222222222222".into(),
    };
    let db_thread = agent::DbThread {
        title: "Review thread".into(),
        messages: Vec::new(),
        updated_at: Utc::now(),
        detailed_summary: None,
        initial_project_snapshot: None,
        cumulative_token_usage: Default::default(),
        request_token_usage: HashMap::default(),
        model: None,
        profile: None,
        subagent_context: None,
        speed: None,
        thinking_enabled: false,
        thinking_effort: None,
        draft_prompt: None,
        ui_scroll_position: None,
        sandboxed_terminal_temp_dir: None,
        sandbox_grants: Default::default(),
        execution_policy: acp_thread::ThreadExecutionPolicy::ReadOnly,
        stack_review_origin: Some(persisted_origin.clone()),
    };
    let save_task = cx.update(|_window, cx| {
        ThreadStore::global(cx).update(cx, |store, cx| {
            store.save_thread(session_id.clone(), db_thread, PathList::default(), cx)
        })
    });
    save_task.await.expect("persist Stack Review thread");
    cx.run_until_parked();

    let matching_conversation = panel.update_in(&mut cx, |panel, window, cx| {
        panel
            .create_agent_thread_inner(
                Agent::NativeAgent,
                None,
                None,
                Some(session_id.clone()),
                None,
                Some("Review thread".into()),
                None,
                None,
                AgentThreadSource::AgentPanel,
                Some(persisted_origin.clone()),
                window,
                cx,
            )
            .conversation_view
    });
    cx.run_until_parked();
    assert!(
        matching_conversation
            .read_with(&cx, |view, _cx| view.root_thread_view())
            .is_some(),
        "exact Stack Review origin should construct the normal root ThreadView"
    );
    drop(matching_conversation);
    cx.update(|_window, _cx| {});
    cx.run_until_parked();

    let mut requested_origin = persisted_origin;
    requested_origin.head_oid = "3333333333333333333333333333333333333333".into();
    let conversation_view = panel.update_in(&mut cx, |panel, window, cx| {
        panel
            .resume_stack_review_thread(
                Agent::NativeAgent,
                session_id.clone(),
                requested_origin.clone(),
                "Review thread".into(),
                None,
                window,
                cx,
            )
            .expect("initial Stack Review resume")
    });
    let failed_thread_id = conversation_view.read_with(&cx, |view, _cx| view.thread_id);
    cx.run_until_parked();

    assert!(
        conversation_view
            .read_with(&cx, |view, _cx| view.root_thread_view())
            .is_none(),
        "foreign Stack Review origin must fail before constructing ThreadView/composer"
    );
    assert!(
        conversation_view
            .read_with(&cx, |view, _cx| view.load_error())
            .is_some()
    );
    let second_resume = panel.update_in(&mut cx, |panel, window, cx| {
        panel.resume_stack_review_thread(
            Agent::NativeAgent,
            session_id.clone(),
            requested_origin,
            "Review thread".into(),
            None,
            window,
            cx,
        )
    });
    assert!(
        second_resume.is_err(),
        "failed cached resume must be rejected"
    );
    assert!(
        panel.read_with(&cx, |panel, _cx| {
            !panel.retained_threads.contains_key(&failed_thread_id)
        }),
        "failed cached Stack Review conversation must be evicted"
    );
    let native_connection = panel
        .read_with(&cx, |panel, cx| {
            let entry = panel
                .connection_store
                .read(cx)
                .entry(&Agent::NativeAgent)?
                .clone();
            match entry.read(cx) {
                crate::agent_connection_store::AgentConnectionEntry::Connected(state) => state
                    .connection
                    .clone()
                    .downcast::<agent::NativeAgentConnection>(),
                crate::agent_connection_store::AgentConnectionEntry::Connecting { .. }
                | crate::agent_connection_store::AgentConnectionEntry::Error { .. } => None,
            }
        })
        .expect("native Agent connection");
    assert!(
        panel.read_with(&cx, |_panel, cx| {
            native_connection.thread(&session_id, cx).is_none()
        }),
        "rejected Stack Review resume must release its native session reference"
    );
}

#[gpui::test]
async fn test_stack_review_ai_host_uses_workspace_owned_agent_thread(cx: &mut TestAppContext) {
    use git_ui_core::stack_review_ai::{
        StackReviewAiActivationRequest, StackReviewAiContext, StackReviewAiContextKey,
        StackReviewAiGeneration, StackReviewAiHost as _, StackReviewAiResource,
        StackReviewAiStatus, StackReviewAiSubmitRequest, StackReviewCitationNavigationRequest,
    };

    let (workspace, panel, mut cx) = setup_workspace_panel(cx).await;
    let host = crate::stack_review_ai::ZedStackReviewAiHost;
    assert_eq!(std::mem::size_of_val(&host), 0);
    let citation = StackReviewCitationNavigationRequest::try_new(
        "base-head",
        "project-identity",
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        Some("src/main.rs".into()),
        git::stack_review::StackReviewCommentSide::Right,
        Some(1..=1),
        Some("comment-a".into()),
        Some("comment-a".into()),
    )
    .expect("valid Stack Review citation");
    let canceled_generation = StackReviewAiGeneration::default()
        .next()
        .expect("first generation");
    let context = StackReviewAiContext {
        key: StackReviewAiContextKey::comment("base-head", "thread-a"),
        context_revision: "revision-a".into(),
        title: "Review · src/main.rs".into(),
        base_oid: "1111111111111111111111111111111111111111".into(),
        head_oid: "2222222222222222222222222222222222222222".into(),
        path: Some("src/main.rs".into()),
        side: Some(git::stack_review::StackReviewCommentSide::Right),
        line_range: Some(0..1),
        selected_record_id: Some("comment-a".into()),
        resources: vec![StackReviewAiResource::new("src/main.rs", citation, "code")].into(),
    };
    let canceled_activation = cx.update(|window, cx| {
        host.activate_context(
            StackReviewAiActivationRequest {
                context: context.clone(),
                persisted_session_id: None,
                generation: canceled_generation,
                prepare_composer: false,
            },
            workspace.downgrade(),
            window,
            cx,
        )
    });
    drop(canceled_activation);
    cx.run_until_parked();
    assert!(
        panel.read_with(&cx, |panel, _cx| panel.retained_threads().is_empty()),
        "dropping an unpolled activation must not retain a conversation"
    );

    let generation = canceled_generation.next().expect("second generation");
    let activation_task = cx.update(|window, cx| {
        host.activate_context(
            StackReviewAiActivationRequest {
                context: context.clone(),
                persisted_session_id: None,
                generation,
                prepare_composer: true,
            },
            workspace.downgrade(),
            window,
            cx,
        )
    });
    let projection = activation_task
        .await
        .expect("activate Stack Review Agent context");
    cx.run_until_parked();

    let (session_id, session_owner) = projection.read_with(&cx, |projection, _cx| {
        assert_eq!(projection.status(), &StackReviewAiStatus::Ready);
        (
            projection
                .session_id()
                .cloned()
                .expect("loaded Stack Review Agent session"),
            projection
                .session_owner()
                .cloned()
                .expect("loaded Stack Review Agent owner"),
        )
    });
    panel.read_with(&cx, |panel, _cx| {
        assert_eq!(panel.retained_threads().len(), 1);
    });
    let (draft_blocks, entries_empty) = panel.read_with(&cx, |panel, cx| {
        let thread_view = panel
            .retained_threads()
            .values()
            .next()
            .expect("retained Stack Review conversation")
            .read(cx)
            .root_thread_view()
            .expect("loaded Stack Review ThreadView");
        let draft_blocks = thread_view
            .read(cx)
            .message_editor
            .read(cx)
            .draft_content_blocks_snapshot(cx);
        let entries_empty = thread_view.read(cx).thread.read(cx).entries().is_empty();
        (draft_blocks, entries_empty)
    });
    assert!(
        entries_empty,
        "preparing context must not contact the Agent"
    );
    assert!(
        crate::stack_review_ai::stack_review_turn_envelope(
            &draft_blocks,
            &context.key,
            &"project-identity".into(),
        )
        .is_some(),
        "Use File must populate the ordinary Agent composer with canonical context"
    );

    let foreign_owner_error = cx.update(|window, cx| {
        host.reveal_and_focus_thread(
            &session_id,
            &"native".into(),
            workspace.downgrade(),
            window,
            cx,
        )
        .expect_err("Agent kind must not substitute for the exact conversation owner")
    });
    assert!(
        foreign_owner_error
            .to_string()
            .contains("session is unavailable")
    );

    cx.update(|window, cx| {
        host.reveal_and_focus_thread(
            &session_id,
            &session_owner,
            workspace.downgrade(),
            window,
            cx,
        )
        .expect("reveal Stack Review Agent thread");
    });
    panel.read_with(&cx, |panel, cx| {
        assert!(panel.active_agent_thread(cx).is_some());
    });
    let original_conversation_id = panel.read_with(&cx, |panel, cx| {
        let session_id = acp::SessionId::new(session_id.to_string());
        let thread_id = panel
            .thread_id_for_session(&session_id, cx)
            .expect("Stack Review panel thread");
        panel
            .conversation_view_for_id(&thread_id, cx)
            .expect("Stack Review ConversationView")
            .entity_id()
    });

    panel.update(&mut cx, |panel, cx| {
        panel.set_selected_agent_and_persist(
            Agent::Custom {
                id: agent_servers::CLAUDE_AGENT_ID.into(),
            },
            cx,
        );
    });

    let next_generation = generation.next().expect("second generation");
    let resume_task = cx.update(|window, cx| {
        host.activate_context(
            StackReviewAiActivationRequest {
                context: context.clone(),
                persisted_session_id: Some(session_id.clone()),
                generation: next_generation,
                prepare_composer: false,
            },
            workspace.downgrade(),
            window,
            cx,
        )
    });
    let resumed_projection = resume_task
        .await
        .expect("resume Stack Review Agent context");
    cx.run_until_parked();
    resumed_projection.read_with(&cx, |projection, _cx| {
        assert_eq!(projection.generation(), next_generation);
        assert_eq!(projection.session_id(), Some(&session_id));
        assert_eq!(projection.status(), &StackReviewAiStatus::Ready);
    });
    panel.read_with(&cx, |panel, cx| {
        let native_session_id = acp::SessionId::new(session_id.to_string());
        let thread_id = panel
            .thread_id_for_session(&native_session_id, cx)
            .expect("resumed Stack Review panel thread");
        assert_eq!(
            panel
                .conversation_view_for_id(&thread_id, cx)
                .expect("resumed Stack Review ConversationView")
                .entity_id(),
            original_conversation_id,
            "persisted activation must reuse the exact panel-owned ConversationView"
        );
        assert_eq!(
            panel
                .conversation_view_for_id(&thread_id, cx)
                .expect("resumed Stack Review ConversationView")
                .read(cx)
                .agent_key(),
            &Agent::NativeAgent,
            "durable Stack Review bindings must ignore the selected external Agent"
        );
        assert_eq!(
            panel
                .active_agent_thread(cx)
                .expect("active Stack Review Agent thread")
                .read(cx)
                .session_id()
                .to_string(),
            session_id.as_ref()
        );
    });

    let message_editor = panel
        .read_with(&cx, |panel, cx| panel.active_thread_view(cx))
        .expect("active Stack Review ThreadView")
        .read_with(&cx, |thread_view, _cx| thread_view.message_editor.clone());
    message_editor.update_in(&mut cx, |editor, window, cx| {
        editor.set_text("keep this draft", window, cx);
    });
    cx.update(|window, cx| {
        host.prepare_composer(
            context.clone(),
            &session_id,
            &session_owner,
            workspace.downgrade(),
            window,
            cx,
        )
        .expect("replace Stack Review composer context");
    });
    let draft_blocks =
        message_editor.read_with(&cx, |editor, cx| editor.draft_content_blocks_snapshot(cx));
    let draft_user_text = draft_blocks
        .iter()
        .filter_map(|block| match block {
            acp::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(draft_user_text, "keep this draft");
    assert_eq!(
        draft_blocks
            .iter()
            .filter(|block| crate::stack_review_ai::is_stack_review_context_block(block))
            .count(),
        context.resources.len() + 1,
        "composer replacement must retain exactly one resource set and envelope"
    );
    assert!(
        crate::stack_review_ai::stack_review_turn_envelope(
            &draft_blocks,
            &context.key,
            &"project-identity".into(),
        )
        .is_some()
    );
    let submit_generation = next_generation.next().expect("submit generation");
    let submission = StackReviewAiSubmitRequest::new(
        context.clone(),
        session_id.clone(),
        session_owner,
        "Please review this comment.",
        "turn-1",
        "project-identity",
        Some("comment-a".into()),
        submit_generation,
    );
    let submit_task = cx.update(|window, cx| {
        host.submit_local_prompt(submission, workspace.downgrade(), window, cx)
    });
    submit_task.await.expect("submit Stack Review Agent prompt");
    cx.run_until_parked();

    let remaining_draft_text = message_editor
        .read_with(&cx, |editor, cx| editor.draft_content_blocks_snapshot(cx))
        .into_iter()
        .filter_map(|block| match block {
            acp::ContentBlock::Text(text) => Some(text.text),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(remaining_draft_text, "keep this draft");
    let chunks = panel.read_with(&cx, |panel, cx| {
        panel
            .active_agent_thread(cx)
            .expect("active Stack Review Agent thread")
            .read(cx)
            .entries()
            .iter()
            .rev()
            .find_map(|entry| entry.user_message())
            .expect("persisted Stack Review user turn")
            .chunks
            .clone()
    });
    assert!(matches!(
        chunks.first(),
        Some(acp::ContentBlock::Text(text)) if text.text == "Please review this comment."
    ));
    let embedded_text = chunks
        .iter()
        .filter_map(|block| match block {
            acp::ContentBlock::Resource(resource) => match &resource.resource {
                acp::EmbeddedResourceResource::TextResourceContents(resource) => {
                    Some(resource.text.as_str())
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(embedded_text.len(), context.resources.len() + 1);
    let project_identity: SharedString = "project-identity".into();
    let envelope = crate::stack_review_ai::stack_review_turn_envelope(
        &chunks,
        &context.key,
        &project_identity,
    )
    .expect("strict Stack Review turn envelope metadata");
    assert_eq!(envelope.turn_id().as_ref(), "turn-1");
    assert_eq!(envelope.context_key(), &context.key);
    let mut duplicate_envelope_chunks = chunks.clone();
    duplicate_envelope_chunks.push(acp::ContentBlock::Text(acp::TextContent::new(
        envelope
            .to_metadata_text()
            .expect("duplicate turn metadata"),
    )));
    assert!(
        crate::stack_review_ai::stack_review_turn_envelope(
            &duplicate_envelope_chunks,
            &context.key,
            &project_identity,
        )
        .is_none(),
        "duplicate metadata must fail closed"
    );

    let acp_thread = panel
        .read_with(&cx, |panel, cx| panel.active_agent_thread(cx))
        .expect("active Stack Review ACP thread");
    acp_thread.update(&mut cx, |thread, cx| {
        thread.push_assistant_content_block("assistant response".into(), false, cx);
    });
    cx.run_until_parked();
    let native_markdown = acp_thread.read_with(&cx, |thread, _cx| {
        thread
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                acp_thread::AgentThreadEntry::AssistantMessage(message) => {
                    message.chunks.iter().find_map(|chunk| match chunk {
                        acp_thread::AssistantMessageChunk::Message { block, .. } => {
                            block.markdown().cloned()
                        }
                        acp_thread::AssistantMessageChunk::Thought { .. } => None,
                    })
                }
                _ => None,
            })
            .expect("native assistant Markdown entity")
    });
    resumed_projection.read_with(&cx, |projection, _cx| {
        assert_eq!(projection.assistant_turns().len(), 1);
        assert_eq!(
            projection.assistant_turns()[0].markdown().entity_id(),
            native_markdown.entity_id(),
            "inline projection must reuse the native AcpThread Markdown entity"
        );
    });
}
