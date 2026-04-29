use super::App;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::text_formatting::truncate_text;
use chrono::Utc;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadSourceKind;
use codex_protocol::ThreadId;
use ratatui::style::Stylize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchNavigatorRole {
    Main,
    Ancestor,
    Parent,
    Current,
    Child,
}

struct BranchNavigatorRow {
    thread_id: ThreadId,
    depth: Option<u32>,
    role: BranchNavigatorRole,
    anchor_head_summary: Option<String>,
    anchor_summary: Option<String>,
    updated_at: Option<i64>,
}

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

        let mut rows = match self
            .branch_ancestor_rows(app_server, current_thread_id)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load branch ancestry: {err}"));
                return;
            }
        };
        let current_thread = match app_server
            .thread_read(current_thread_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => thread,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load current branch: {err}"));
                return;
            }
        };
        let current_row_index = rows.len();
        rows.push(BranchNavigatorRow {
            thread_id: current_thread_id,
            depth: current_thread.branch_depth.or_else(|| {
                Some(u32::try_from(self.chat_widget.branch_depth()).unwrap_or(u32::MAX))
            }),
            role: BranchNavigatorRole::Current,
            anchor_head_summary: current_thread.branch_anchor_head_summary,
            anchor_summary: current_thread.branch_anchor_summary.or_else(|| {
                self.chat_widget
                    .branch_anchor_summary()
                    .map(ToString::to_string)
            }),
            updated_at: Some(current_thread.updated_at),
        });

        let children = match self
            .direct_child_branch_rows(app_server, current_thread_id)
            .await
        {
            Ok(children) => children,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to list branches: {err}"));
                return;
            }
        };
        rows.extend(children);
        normalize_branch_navigator_rows(&mut rows, current_row_index);

        let mut items = Vec::with_capacity(rows.len());
        let now = Utc::now().timestamp();
        for row in rows {
            let thread_id = row.thread_id;
            let thread_id_text = thread_id.to_string();
            let depth = row.depth.unwrap_or(0);
            let anchor = branch_anchor_label(
                row.role,
                row.anchor_head_summary.as_deref(),
                row.anchor_summary.as_deref(),
            );
            let name = branch_navigator_name(depth, row.role, row.updated_at, now, &thread_id_text);
            let search_value = format!("{name} {} {anchor}", branch_role_label(row.role));
            items.push(SelectionItem {
                name,
                detail: Some(anchor),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::ResumeSessionByIdOrName(thread_id_text.clone()));
                })],
                dismiss_on_select: true,
                search_value: Some(search_value),
                ..Default::default()
            });
        }

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Branch navigator".to_string()),
            subtitle: Some("Flat branch list for the current conversation".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(current_row_index),
            ..Default::default()
        });
    }

    async fn branch_ancestor_rows(
        &mut self,
        app_server: &mut AppServerSession,
        current_thread_id: ThreadId,
    ) -> color_eyre::Result<Vec<BranchNavigatorRow>> {
        let mut parent_id = self.chat_widget.branch_parent_thread_id();
        let mut ancestors: Vec<BranchNavigatorRow> = Vec::new();
        while let Some(thread_id) = parent_id {
            if thread_id == current_thread_id
                || ancestors.iter().any(|row| row.thread_id == thread_id)
            {
                break;
            }
            let thread = app_server
                .thread_read(thread_id, /*include_turns*/ false)
                .await?;
            parent_id = thread
                .forked_from_id
                .as_deref()
                .and_then(|id| ThreadId::from_string(id).ok());
            ancestors.push(BranchNavigatorRow {
                thread_id,
                depth: thread.branch_depth,
                role: BranchNavigatorRole::Ancestor,
                anchor_head_summary: thread.branch_anchor_head_summary,
                anchor_summary: thread.branch_anchor_summary,
                updated_at: Some(thread.updated_at),
            });
        }

        ancestors.reverse();
        Ok(ancestors)
    }

    async fn direct_child_branch_rows(
        &mut self,
        app_server: &mut AppServerSession,
        current_thread_id: ThreadId,
    ) -> color_eyre::Result<Vec<BranchNavigatorRow>> {
        let current_thread_id_text = current_thread_id.to_string();
        let mut cursor = None;
        let mut rows = Vec::new();
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
                .await?;

            rows.extend(
                response
                    .data
                    .into_iter()
                    .filter(|thread| {
                        thread.forked_from_id.as_deref() == Some(current_thread_id_text.as_str())
                    })
                    .filter_map(child_branch_row),
            );
            cursor = response.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(rows)
    }
}

fn child_branch_row(thread: Thread) -> Option<BranchNavigatorRow> {
    Some(BranchNavigatorRow {
        thread_id: ThreadId::from_string(&thread.id).ok()?,
        depth: thread.branch_depth,
        role: BranchNavigatorRole::Child,
        anchor_head_summary: thread.branch_anchor_head_summary,
        anchor_summary: thread.branch_anchor_summary,
        updated_at: Some(thread.updated_at),
    })
}

fn normalize_branch_navigator_rows(rows: &mut [BranchNavigatorRow], current_row_index: usize) {
    let current_depth = rows
        .get(current_row_index)
        .and_then(|row| row.depth)
        .unwrap_or_else(|| u32::try_from(current_row_index).unwrap_or(u32::MAX));
    for (index, row) in rows.iter_mut().enumerate() {
        row.depth = Some(match index.cmp(&current_row_index) {
            std::cmp::Ordering::Less => {
                let steps_to_current = u32::try_from(current_row_index - index).unwrap_or(u32::MAX);
                current_depth.saturating_sub(steps_to_current)
            }
            std::cmp::Ordering::Equal => current_depth,
            std::cmp::Ordering::Greater => current_depth.saturating_add(1),
        });
        row.role = match index.cmp(&current_row_index) {
            std::cmp::Ordering::Less if index == 0 => BranchNavigatorRole::Main,
            std::cmp::Ordering::Less if index + 1 == current_row_index => {
                BranchNavigatorRole::Parent
            }
            std::cmp::Ordering::Less => BranchNavigatorRole::Ancestor,
            std::cmp::Ordering::Equal => BranchNavigatorRole::Current,
            std::cmp::Ordering::Greater => BranchNavigatorRole::Child,
        };
    }
}

fn branch_role_label(role: BranchNavigatorRole) -> &'static str {
    match role {
        BranchNavigatorRole::Main => "main",
        BranchNavigatorRole::Ancestor => "ancestor",
        BranchNavigatorRole::Parent => "parent",
        BranchNavigatorRole::Current => "current",
        BranchNavigatorRole::Child => "child",
    }
}

fn branch_relation_glyph(role: BranchNavigatorRole) -> &'static str {
    match role {
        BranchNavigatorRole::Main => "•",
        BranchNavigatorRole::Ancestor | BranchNavigatorRole::Parent => "↑",
        BranchNavigatorRole::Current => "→",
        BranchNavigatorRole::Child => "⎇",
    }
}

fn branch_navigator_name(
    depth: u32,
    role: BranchNavigatorRole,
    updated_at: Option<i64>,
    now: i64,
    thread_id: &str,
) -> String {
    let relation = branch_relation_glyph(role);
    let updated = format_compact_updated(updated_at, now);
    format!("d{depth} {relation} · {updated} · {thread_id}")
}

fn format_compact_updated(updated_at: Option<i64>, now: i64) -> String {
    let Some(updated_at) = updated_at else {
        return "-".to_string();
    };
    let secs = now.saturating_sub(updated_at).max(0);
    if secs < 5 {
        "now".to_string()
    } else if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        format!("{}m", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}h", secs / (60 * 60))
    } else {
        format!("{}d", secs / (60 * 60 * 24))
    }
}

fn branch_anchor_label(
    role: BranchNavigatorRole,
    anchor_head_summary: Option<&str>,
    anchor_summary: Option<&str>,
) -> String {
    if role == BranchNavigatorRole::Main {
        return "root".to_string();
    }
    if let Some(anchor_head_summary) = anchor_head_summary
        && !anchor_head_summary.trim().is_empty()
    {
        let head_summary = truncate_text(anchor_head_summary.trim(), 64);
        let suffix = if head_summary.ends_with("...") {
            ""
        } else {
            "..."
        };
        return format!("\"{head_summary}{suffix}\"");
    }
    match anchor_summary {
        Some(anchor_summary) if !anchor_summary.trim().is_empty() => {
            format!("\"...{}\"", truncate_text(anchor_summary.trim(), 64))
        }
        _ => "\"selected point\"".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_anchor_label_uses_root_for_main() {
        assert_eq!(
            branch_anchor_label(BranchNavigatorRole::Main, Some("ignored"), None),
            "root"
        );
    }

    #[test]
    fn branch_anchor_label_uses_trailing_ellipsis_for_head_summary() {
        assert_eq!(
            branch_anchor_label(
                BranchNavigatorRole::Child,
                Some("Fee structure: monthly retainer amount"),
                Some("payment terms")
            ),
            "\"Fee structure: monthly retainer amount...\""
        );
    }

    #[test]
    fn branch_anchor_label_truncates_head_summary_without_double_ellipsis() {
        assert_eq!(
            branch_anchor_label(
                BranchNavigatorRole::Child,
                Some("retainer contract terms and fee structure details that keep going"),
                None
            ),
            "\"retainer contract terms and fee structure details that keep g...\""
        );
    }

    #[test]
    fn branch_anchor_label_uses_leading_ellipsis_for_tail_summary() {
        assert_eq!(
            branch_anchor_label(
                BranchNavigatorRole::Child,
                None,
                Some("retainer contract terms and fee structure details that keep going")
            ),
            "\"...retainer contract terms and fee structure details that keep g...\""
        );
    }

    #[test]
    fn normalize_branch_navigator_rows_labels_relative_roles_and_depths() {
        let ids: Vec<ThreadId> = (0..5).map(|_| ThreadId::new()).collect();
        let mut rows = vec![
            BranchNavigatorRow {
                thread_id: ids[0],
                depth: None,
                role: BranchNavigatorRole::Ancestor,
                anchor_head_summary: None,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[1],
                depth: None,
                role: BranchNavigatorRole::Ancestor,
                anchor_head_summary: None,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[2],
                depth: Some(2),
                role: BranchNavigatorRole::Current,
                anchor_head_summary: None,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[3],
                depth: None,
                role: BranchNavigatorRole::Child,
                anchor_head_summary: None,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[4],
                depth: Some(42),
                role: BranchNavigatorRole::Child,
                anchor_head_summary: None,
                anchor_summary: None,
                updated_at: None,
            },
        ];

        normalize_branch_navigator_rows(&mut rows, /*current_row_index*/ 2);

        let roles_and_depths: Vec<_> = rows.iter().map(|row| (row.role, row.depth)).collect();
        assert_eq!(
            roles_and_depths,
            vec![
                (BranchNavigatorRole::Main, Some(0)),
                (BranchNavigatorRole::Parent, Some(1)),
                (BranchNavigatorRole::Current, Some(2)),
                (BranchNavigatorRole::Child, Some(3)),
                (BranchNavigatorRole::Child, Some(3)),
            ]
        );
    }

    #[test]
    fn branch_relation_glyphs_are_compact() {
        assert_eq!(branch_relation_glyph(BranchNavigatorRole::Main), "•");
        assert_eq!(branch_relation_glyph(BranchNavigatorRole::Parent), "↑");
        assert_eq!(branch_relation_glyph(BranchNavigatorRole::Current), "→");
        assert_eq!(branch_relation_glyph(BranchNavigatorRole::Child), "⎇");
    }

    #[test]
    fn format_compact_updated_uses_short_units() {
        let now = 1_000_000;
        assert_eq!(format_compact_updated(Some(now), now), "now");
        assert_eq!(format_compact_updated(Some(now - 42), now), "42s");
        assert_eq!(format_compact_updated(Some(now - 120), now), "2m");
        assert_eq!(format_compact_updated(Some(now - 7_200), now), "2h");
        assert_eq!(format_compact_updated(Some(now - 172_800), now), "2d");
        assert_eq!(format_compact_updated(None, now), "-");
    }

    #[test]
    fn branch_navigator_name_uses_full_thread_id() {
        let thread_id = "019db0d3-dd1e-71b1-a5d9-5248aaf2d19d";
        assert_eq!(
            branch_navigator_name(
                3,
                BranchNavigatorRole::Current,
                Some(1_000_000 - 240),
                1_000_000,
                thread_id
            ),
            "d3 → · 4m · 019db0d3-dd1e-71b1-a5d9-5248aaf2d19d"
        );
    }
}
