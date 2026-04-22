use super::App;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadSourceKind;
use ratatui::style::Stylize;

impl App {
    pub(super) fn show_branch_info(&mut self) {
        let Some(current_thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Branch info is unavailable before the current conversation has started."
                    .to_string(),
            );
            return;
        };

        let branch_depth = self.chat_widget.branch_depth();
        if branch_depth == 0 {
            self.chat_widget.add_plain_history_lines(vec![
                "This conversation is not currently in a branch.".into(),
                vec!["current: ".into(), current_thread_id.to_string().cyan()].into(),
            ]);
            return;
        }

        let anchor_line = match self.chat_widget.branch_anchor_summary() {
            Some(anchor_summary) if !anchor_summary.trim().is_empty() => vec![
                "anchor: ".into(),
                format!("\"...{}\"", anchor_summary.trim()).cyan(),
            ]
            .into(),
            _ => "anchor: unavailable".into(),
        };
        let mut lines = vec![
            vec!["branch depth: ".into(), branch_depth.to_string().cyan()].into(),
            anchor_line,
        ];
        if let Some(parent_thread_id) = self.chat_widget.branch_parent_thread_id() {
            lines.push(vec!["parent: ".into(), parent_thread_id.to_string().cyan()].into());
        } else {
            lines.push("parent: unavailable".into());
        }
        lines.push(vec!["current: ".into(), current_thread_id.to_string().cyan()].into());
        self.chat_widget.add_plain_history_lines(lines);
    }

    pub(super) async fn show_branch_list(&mut self, app_server: &mut AppServerSession) {
        let Some(current_thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Branch list is unavailable before the current conversation has started."
                    .to_string(),
            );
            return;
        };

        let current_thread_id_text = current_thread_id.to_string();
        let mut cursor = None;
        let mut children = Vec::new();
        loop {
            let response = app_server
                .thread_list(ThreadListParams {
                    cursor,
                    limit: Some(100),
                    sort_key: Some(ThreadSortKey::UpdatedAt),
                    sort_direction: None,
                    model_providers: None,
                    source_kinds: Some(vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode]),
                    archived: Some(false),
                    cwd: None,
                    search_term: None,
                })
                .await;
            let response = match response {
                Ok(response) => response,
                Err(err) => {
                    self.chat_widget
                        .add_error_message(format!("Failed to list branches: {err}"));
                    return;
                }
            };

            children.extend(response.data.into_iter().filter(|thread| {
                thread.forked_from_id.as_deref() == Some(current_thread_id_text.as_str())
            }));
            cursor = response.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        if children.is_empty() {
            self.chat_widget.add_plain_history_lines(vec![
                "No child branches found for this conversation.".into(),
                vec!["current: ".into(), current_thread_id.to_string().cyan()].into(),
            ]);
            return;
        }

        let mut lines = vec!["Child branches:".into()];
        for child in children {
            let level = child
                .branch_depth
                .map(|depth| depth.to_string())
                .unwrap_or_else(|| "?".to_string());
            let anchor = match child.branch_anchor_summary.as_deref() {
                Some(anchor_summary) if !anchor_summary.trim().is_empty() => {
                    format!("\"...{}\"", anchor_summary.trim())
                }
                _ => "\"selected point\"".to_string(),
            };
            lines.push(
                vec![
                    level.cyan(),
                    " - ".into(),
                    anchor.into(),
                    " - ".into(),
                    child.id.cyan(),
                ]
                .into(),
            );
        }
        lines.push(
            "Use /resume <conversation-id> to switch to a listed branch."
                .dim()
                .into(),
        );
        self.chat_widget.add_plain_history_lines(lines);
    }
}
