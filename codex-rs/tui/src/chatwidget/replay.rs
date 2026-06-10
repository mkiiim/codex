//! Thread replay rendering for `ChatWidget`.
//!
//! This module rehydrates turns and items into transcript state while avoiding
//! live-only side effects.

use super::*;

impl ChatWidget {
    fn replay_completed_agent_message(
        &mut self,
        text: String,
        source_text: Option<String>,
        source_segments: Option<Vec<String>>,
        phase: Option<MessagePhase>,
    ) {
        let raw_markdown = source_text.filter(|source| !source.trim().is_empty());
        let render_source = raw_markdown.as_deref().unwrap_or(text.as_str());
        let parsed = parse_assistant_markdown(render_source, self.config.cwd.as_path());
        let visible_markdown = parsed.visible_markdown;
        let mut lines = Vec::new();
        crate::markdown::append_markdown(
            &visible_markdown,
            /*width*/ None,
            Some(self.config.cwd.as_path()),
            &mut lines,
        );
        let mut cell = history_cell::AgentMessageCell::new_with_raw_markdown(
            lines,
            /*is_first_line*/ true,
            raw_markdown.clone(),
        );
        cell.set_source_segments(source_segments);
        self.add_to_history(cell);
        if matches!(phase, Some(MessagePhase::FinalAnswer) | None) {
            if let Some(raw_markdown) = raw_markdown {
                self.record_agent_markdown(&raw_markdown);
            } else if visible_markdown.is_empty() {
                self.last_agent_markdown = None;
            } else {
                self.record_agent_markdown(&visible_markdown);
            }
        }
        self.request_redraw();
    }

    /// Replay a subset of initial events into the UI to seed the transcript when
    /// resuming an existing session. This approximates the live event flow and
    /// is intentionally conservative: only safe-to-replay items are rendered to
    /// avoid triggering side effects. Event ids are passed as `None` to
    /// distinguish replayed events from live ones.
    pub(crate) fn replay_thread_turns(&mut self, turns: Vec<Turn>, replay_kind: ReplayKind) {
        for turn in turns {
            let Turn {
                id: turn_id,
                items_view: _,
                items,
                status,
                error,
                started_at,
                completed_at,
                duration_ms,
            } = turn;
            if matches!(status, TurnStatus::InProgress) {
                self.last_non_retry_error = None;
                self.on_task_started();
            }
            for item in items {
                self.replay_thread_item(item, turn_id.clone(), replay_kind);
            }
            if matches!(
                status,
                TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
            ) {
                self.handle_turn_completed_notification(
                    TurnCompletedNotification {
                        thread_id: self.thread_id.map(|id| id.to_string()).unwrap_or_default(),
                        turn: Turn {
                            id: turn_id,
                            items_view: codex_app_server_protocol::TurnItemsView::NotLoaded,
                            items: Vec::new(),
                            status,
                            error,
                            started_at,
                            completed_at,
                            duration_ms,
                        },
                    },
                    Some(replay_kind),
                );
            }
        }
    }

    pub(crate) fn replay_thread_item(
        &mut self,
        item: ThreadItem,
        turn_id: String,
        replay_kind: ReplayKind,
    ) {
        self.handle_thread_item(item, turn_id, ThreadItemRenderSource::Replay(replay_kind));
    }

    pub(super) fn handle_thread_item(
        &mut self,
        item: ThreadItem,
        turn_id: String,
        render_source: ThreadItemRenderSource,
    ) {
        let from_replay = render_source.is_replay();
        let replay_kind = render_source.replay_kind();
        match item {
            ThreadItem::UserMessage { content, .. } => {
                self.on_committed_user_message(&content, from_replay);
            }
            ThreadItem::AgentMessage {
                id,
                text,
                source_text,
                source_segments,
                phase,
                memory_citation,
            } => {
                if from_replay {
                    let _ = id;
                    let _ = memory_citation;
                    self.replay_completed_agent_message(text, source_text, source_segments, phase);
                } else {
                    self.on_agent_message_item_completed(
                        AgentMessageItem {
                            id,
                            content: vec![AgentMessageContent::Text { text }],
                            phase,
                            memory_citation: memory_citation.map(|citation| {
                                codex_protocol::memory_citation::MemoryCitation {
                                    entries: citation
                                        .entries
                                        .into_iter()
                                        .map(|entry| {
                                            codex_protocol::memory_citation::MemoryCitationEntry {
                                                path: entry.path,
                                                line_start: entry.line_start,
                                                line_end: entry.line_end,
                                                note: entry.note,
                                            }
                                        })
                                        .collect(),
                                    rollout_ids: citation.thread_ids,
                                }
                            }),
                        },
                        /*from_replay*/ false,
                    );
                }
            }
            ThreadItem::Plan { text, .. } => self.on_plan_item_completed(text),
            ThreadItem::Reasoning {
                summary, content, ..
            } => {
                if from_replay {
                    for delta in summary {
                        self.on_agent_reasoning_delta(delta);
                    }
                    if self.config.show_raw_agent_reasoning {
                        for delta in content {
                            self.on_agent_reasoning_delta(delta);
                        }
                    }
                }
                self.on_agent_reasoning_final();
            }
            item @ ThreadItem::CommandExecution {
                status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                ..
            } => self.on_command_execution_started(item),
            item @ ThreadItem::CommandExecution { .. } => self.on_command_execution_completed(item),
            ThreadItem::FileChange {
                status: codex_app_server_protocol::PatchApplyStatus::InProgress,
                ..
            } => {}
            item @ ThreadItem::FileChange { .. } => self.on_file_change_completed(item),
            item @ ThreadItem::McpToolCall {
                status: codex_app_server_protocol::McpToolCallStatus::InProgress,
                ..
            } => self.on_mcp_tool_call_started(item),
            item @ ThreadItem::McpToolCall { .. } => self.on_mcp_tool_call_completed(item),
            ThreadItem::WebSearch { id, query, action } => {
                self.on_web_search_begin(id.clone());
                self.on_web_search_end(
                    id,
                    query,
                    action.unwrap_or(codex_app_server_protocol::WebSearchAction::Other),
                );
            }
            ThreadItem::ImageView { id: _, path } => {
                self.on_view_image_tool_call(path);
            }
            ThreadItem::ImageGeneration {
                id,
                revised_prompt,
                saved_path,
                ..
            } => {
                self.on_image_generation_end(id, revised_prompt, saved_path);
            }
            ThreadItem::EnteredReviewMode { review, .. } => {
                if from_replay {
                    self.enter_review_mode_with_hint(review, /*from_replay*/ true);
                }
            }
            ThreadItem::ExitedReviewMode { .. } => {
                self.exit_review_mode_after_item();
            }
            ThreadItem::ContextCompaction { .. } => {
                self.add_info_message("Context compacted".to_string(), /*hint*/ None);
            }
            ThreadItem::HookPrompt { .. } => {}
            ThreadItem::CollabAgentToolCall {
                id,
                tool,
                status,
                sender_thread_id,
                receiver_thread_ids,
                prompt,
                model,
                reasoning_effort,
                agents_states,
            } => self.on_collab_agent_tool_call(ThreadItem::CollabAgentToolCall {
                id,
                tool,
                status,
                sender_thread_id,
                receiver_thread_ids,
                prompt,
                model,
                reasoning_effort,
                agents_states,
            }),
            item @ ThreadItem::SubAgentActivity { .. } => self.on_sub_agent_activity(item),
            ThreadItem::DynamicToolCall { .. } => {}
        }

        if matches!(replay_kind, Some(ReplayKind::ThreadSnapshot)) && turn_id.is_empty() {
            self.request_redraw();
        }
    }
}
