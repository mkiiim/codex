//! Reply-level branch-from dispatch for `App`.
//!
//! Handles `AppEvent::StartBranchFrom` (snippet → locate anchor → fork thread) and
//! `AppEvent::ReturnFromBranch` (navigate back to the parent thread).

use super::App;
use super::AppRunControl;
use crate::app_event::BranchNavigatorSelectionKind;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::BranchForkContext;
use crate::resume_picker::SessionTarget;
use crate::branch_chrome::BranchStateNoticeKind;
use crate::branch_locator::BranchSnippetError;
use crate::branch_locator::locate_branch_snippet;
use crate::tui;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;

impl App {
    pub(super) async fn handle_start_branch_from(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut crate::app_server_session::AppServerSession,
        snippet: String,
    ) {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "/branch-from is unavailable before the session starts.".to_string(),
            );
            return;
        };

        let thread_turns = match app_server
            .thread_read(thread_id, /*include_turns*/ true)
            .await
        {
            Ok(thread) => Some(thread.turns),
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to inspect conversation history for /branch-from: {err}"
                ));
                return;
            }
        };

        let cells = self.transcript_cells.clone();
        let latest_markdown = self.chat_widget.last_agent_markdown.clone();
        let result = locate_branch_snippet(
            &cells,
            thread_turns.as_deref(),
            latest_markdown.as_deref(),
            &snippet,
        );

        match result {
            Err(BranchSnippetError::EmptySnippet) => {
                self.chat_widget
                    .add_error_message("Snippet is empty. Usage: /branch-from <text>".to_string());
            }
            Err(BranchSnippetError::NoAssistantResponse) => {
                self.chat_widget.add_error_message(
                    "/branch-from requires at least one completed assistant response.".to_string(),
                );
            }
            Err(BranchSnippetError::NoMatch) => {
                self.chat_widget.add_error_message(
                    "Snippet not found in any assistant response. Check the text and try again."
                        .to_string(),
                );
            }
            Err(BranchSnippetError::AmbiguousMatch) => {
                self.chat_widget.add_error_message(
                    "Snippet matches multiple assistant responses — provide more context to disambiguate."
                        .to_string(),
                );
            }
            Err(BranchSnippetError::NoAnchor) => {
                self.chat_widget.add_error_message(
                    "Cannot anchor this snippet to a source line. Try selecting a longer or more distinctive excerpt."
                        .to_string(),
                );
            }
            Ok(target) => {
                let parent_branch_depth = self.chat_widget.branch_depth;
                let branch_depth = parent_branch_depth + 1;
                let branch_context = BranchForkContext {
                    snapshot: target.snapshot,
                    origin_snapshot: target.origin_snapshot,
                    branch_depth,
                    selection_summary: target.selection_summary,
                    anchor_head_summary: target.anchor_head_summary,
                    anchor_tail_summary: target.anchor_tail_summary,
                };

                self.refresh_in_memory_config_from_disk_best_effort("branching from reply")
                    .await;

                match app_server
                    .fork_thread_with_snapshot(self.config.clone(), thread_id, branch_context)
                    .await
                {
                    Ok((started, branch_context)) => {
                        self.shutdown_current_thread(app_server).await;
                        match self
                            .replace_chat_widget_with_app_server_thread(
                                tui, app_server, started, /*initial_user_message*/ None,
                            )
                            .await
                        {
                            Ok(()) => {
                                self.chat_widget.branch_depth = branch_context.branch_depth;
                                self.chat_widget.branch_anchor_selection_summary =
                                    branch_context.selection_summary.clone();
                                self.chat_widget.branch_origin_snapshot =
                                    Some(branch_context.origin_snapshot.clone());

                                let notice = BranchStateNoticeKind::BranchFrom {
                                    depth: branch_context.branch_depth,
                                    selection_summary: branch_context.selection_summary,
                                };
                                let title = crate::branch_chrome::branch_state_title(&notice);
                                let body = crate::branch_chrome::branch_state_body(&notice);
                                let mut lines = vec![title];
                                if let Some(body) = body {
                                    lines.push(body);
                                }
                                self.chat_widget.add_plain_history_lines(
                                    lines.into_iter().map(|l| l.into()).collect(),
                                );
                            }
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to attach to branch thread: {err}"
                                ));
                            }
                        }
                    }
                    Err(err) => {
                        self.chat_widget
                            .add_error_message(format!("Failed to create branch thread: {err}"));
                    }
                }
            }
        }

        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn handle_return_from_branch(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut crate::app_server_session::AppServerSession,
    ) {
        let Some(parent_thread_id) = self.chat_widget.parent_thread_id() else {
            self.chat_widget
                .add_error_message("Not in a branch — no parent thread to return to.".to_string());
            tui.frame_requester().schedule_frame();
            return;
        };

        self.refresh_in_memory_config_from_disk_best_effort("returning from branch")
            .await;

        match app_server
            .resume_thread(self.config.clone(), parent_thread_id)
            .await
        {
            Ok(resumed) => {
                let branch_depth = self.chat_widget.branch_depth.saturating_sub(1);
                self.shutdown_current_thread(app_server).await;
                match self
                    .replace_chat_widget_with_app_server_thread(
                        tui, app_server, resumed, /*initial_user_message*/ None,
                    )
                    .await
                {
                    Ok(()) => {
                        let notice = BranchStateNoticeKind::ReturnedToParent {
                            depth: branch_depth + 1,
                        };
                        let title = crate::branch_chrome::branch_state_title(&notice);
                        self.chat_widget.add_plain_history_lines(vec![title.into()]);
                    }
                    Err(err) => {
                        self.chat_widget
                            .add_error_message(format!("Failed to attach to parent thread: {err}"));
                    }
                }
            }
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to return to parent thread: {err}"));
            }
        }

        tui.frame_requester().schedule_frame();
    }

    pub(super) async fn select_branch_navigator_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        kind: BranchNavigatorSelectionKind,
    ) -> Result<AppRunControl> {
        let previous_branch_depth = self.chat_widget.branch_depth;
        let target_session = SessionTarget {
            path: None,
            thread_id,
        };

        match self
            .resume_target_session(tui, app_server, target_session)
            .await?
        {
            AppRunControl::Continue => {}
            AppRunControl::Exit(reason) => return Ok(AppRunControl::Exit(reason)),
        }

        if self.active_thread_id == Some(thread_id)
            && kind == BranchNavigatorSelectionKind::ReturnToAncestor
            && previous_branch_depth > 0
        {
            let notice = BranchStateNoticeKind::ReturnedToParent {
                depth: previous_branch_depth,
            };
            let title = crate::branch_chrome::branch_state_title(&notice);
            self.chat_widget.add_plain_history_lines(vec![title.into()]);
        }

        Ok(AppRunControl::Continue)
    }
}
