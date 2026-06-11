use super::App;
use crate::app_event::AppEvent;
use crate::app_event::BranchNavigatorSelectionKind;
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
use ratatui::text::Line;

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
    anchor_summary: Option<String>,
    updated_at: Option<i64>,
}

impl App {
    pub(super) async fn show_branch_info(&mut self, app_server: &mut AppServerSession) {
        let Some(current_thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Branch info is unavailable before the current conversation has started."
                    .to_string(),
            );
            return;
        };

        let current_thread = match app_server
            .thread_read(current_thread_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => thread,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load branch info: {err}"));
                return;
            }
        };

        self.chat_widget
            .add_plain_history_lines(branch_info_lines(current_thread_id, &current_thread));
    }

    pub(super) async fn show_branch_list(&mut self, app_server: &mut AppServerSession) {
        let Some(current_thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Branch list is unavailable before the current conversation has started."
                    .to_string(),
            );
            return;
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

        let mut rows = match self
            .branch_ancestor_rows(app_server, parent_thread_id(&current_thread))
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load branch ancestry: {err}"));
                return;
            }
        };

        let current_row_index = rows.len();
        rows.push(BranchNavigatorRow {
            thread_id: current_thread_id,
            depth: current_thread.branch_depth,
            role: BranchNavigatorRole::Current,
            anchor_summary: current_thread.branch_anchor_summary.clone(),
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

        self.chat_widget
            .show_selection_view(branch_navigator_selection_params(
                rows,
                current_row_index,
                Utc::now().timestamp(),
            ));
    }

    async fn branch_ancestor_rows(
        &mut self,
        app_server: &mut AppServerSession,
        mut parent_id: Option<ThreadId>,
    ) -> color_eyre::Result<Vec<BranchNavigatorRow>> {
        let mut ancestors = Vec::new();
        while let Some(thread_id) = parent_id {
            if ancestors
                .iter()
                .any(|row: &BranchNavigatorRow| row.thread_id == thread_id)
            {
                break;
            }
            let thread = app_server
                .thread_read(thread_id, /*include_turns*/ false)
                .await?;
            parent_id = parent_thread_id(&thread);
            ancestors.push(BranchNavigatorRow {
                thread_id,
                depth: thread.branch_depth,
                role: BranchNavigatorRole::Ancestor,
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
                    use_state_db_only: false,
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

fn parent_thread_id(thread: &Thread) -> Option<ThreadId> {
    thread
        .forked_from_id
        .as_deref()
        .and_then(|id| ThreadId::from_string(id).ok())
}

fn branch_info_lines(current_thread_id: ThreadId, current_thread: &Thread) -> Vec<Line<'static>> {
    let branch_depth = current_thread.branch_depth.unwrap_or(0);
    if branch_depth == 0 {
        return vec![
            "This conversation is not currently in a branch.".into(),
            vec!["current: ".into(), current_thread_id.to_string().cyan()].into(),
        ];
    }

    let anchor_line = match current_thread.branch_anchor_summary.as_deref() {
        Some(summary) if !summary.trim().is_empty() => vec![
            "anchor: ".into(),
            format!("\"...{}\"", summary.trim()).cyan(),
        ]
        .into(),
        _ => "anchor: unavailable".into(),
    };
    let mut lines = vec![
        vec!["branch depth: ".into(), branch_depth.to_string().cyan()].into(),
        anchor_line,
    ];
    if let Some(parent_thread_id) = parent_thread_id(current_thread) {
        lines.push(vec!["parent: ".into(), parent_thread_id.to_string().cyan()].into());
    } else {
        lines.push("parent: unavailable".into());
    }
    lines.push(vec!["current: ".into(), current_thread_id.to_string().cyan()].into());
    lines
}

fn child_branch_row(thread: Thread) -> Option<BranchNavigatorRow> {
    Some(BranchNavigatorRow {
        thread_id: ThreadId::from_string(&thread.id).ok()?,
        depth: thread.branch_depth,
        role: BranchNavigatorRole::Child,
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

fn branch_anchor_label(role: BranchNavigatorRole, anchor_summary: Option<&str>) -> String {
    if role == BranchNavigatorRole::Main {
        return "root".to_string();
    }
    match anchor_summary {
        Some(s) if !s.trim().is_empty() => {
            format!("\"...{}\"", truncate_text(s.trim(), 64))
        }
        _ => "\"selected point\"".to_string(),
    }
}

fn branch_navigator_selection_params(
    rows: Vec<BranchNavigatorRow>,
    current_row_index: usize,
    now: i64,
) -> SelectionViewParams {
    let items = rows
        .into_iter()
        .map(|row| {
            let thread_id = row.thread_id;
            let thread_id_text = thread_id.to_string();
            let depth = row.depth.unwrap_or(0);
            let anchor = branch_anchor_label(row.role, row.anchor_summary.as_deref());
            let selection_kind = match row.role {
                BranchNavigatorRole::Main
                | BranchNavigatorRole::Ancestor
                | BranchNavigatorRole::Parent => BranchNavigatorSelectionKind::ReturnToAncestor,
                BranchNavigatorRole::Current | BranchNavigatorRole::Child => {
                    BranchNavigatorSelectionKind::OpenCurrentOrDescendant
                }
            };
            let name = branch_navigator_name(depth, row.role, row.updated_at, now, &thread_id_text);
            let search_value = format!("{name} {} {anchor}", branch_role_label(row.role));
            SelectionItem {
                name,
                description: Some(anchor),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectBranchNavigatorThread {
                        thread_id,
                        kind: selection_kind,
                    });
                })],
                dismiss_on_select: true,
                search_value: Some(search_value),
                ..Default::default()
            }
        })
        .collect();

    SelectionViewParams {
        title: Some("Branch navigator".to_string()),
        subtitle: Some("Flat branch list for the current conversation".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        initial_selected_idx: Some(current_row_index),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event_sender::AppEventSender;
    use crate::test_support::PathBufExt;
    use pretty_assertions::assert_eq;

    fn thread_fixture(
        id: &str,
        forked_from_id: Option<&str>,
        branch_depth: Option<u32>,
        branch_anchor_summary: Option<&str>,
        updated_at: i64,
    ) -> Thread {
        Thread {
            id: id.to_string(),
            session_id: id.to_string(),
            forked_from_id: forked_from_id.map(ToString::to_string),
            parent_thread_id: None,
            branch_depth,
            branch_anchor_summary: branch_anchor_summary.map(ToString::to_string),
            branch_anchor_head_summary: None,
            branch_anchor_tail_summary: None,
            branch_origin_snapshot: None,
            preview: "preview".to_string(),
            ephemeral: false,
            model_provider: "openai".to_string(),
            created_at: updated_at,
            updated_at,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: crate::test_support::test_path_buf("/tmp").abs(),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Cli,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: Vec::new(),
        }
    }

    fn branch_info_snapshot(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn branch_info_snapshot_for_child_thread() {
        let current_thread_id =
            ThreadId::from_string("019db89d-d85a-7901-bd8a-fef676f33457").expect("thread id");
        let thread = thread_fixture(
            "019db89d-d85a-7901-bd8a-fef676f33457",
            Some("019db6ce-e9be-7d22-bf8a-3953cfd20e6a"),
            Some(5),
            Some("your case"),
            1_000_000,
        );

        assert_eq!(
            branch_info_snapshot(&branch_info_lines(current_thread_id, &thread)),
            "branch depth: 5\nanchor: \"...your case\"\nparent: 019db6ce-e9be-7d22-bf8a-3953cfd20e6a\ncurrent: 019db89d-d85a-7901-bd8a-fef676f33457"
        );
    }

    #[test]
    fn branch_info_not_in_branch() {
        let current_thread_id =
            ThreadId::from_string("019db89d-d85a-7901-bd8a-fef676f33457").expect("thread id");
        let thread = thread_fixture(
            "019db89d-d85a-7901-bd8a-fef676f33457",
            None,
            Some(0),
            None,
            1_000_000,
        );

        assert_eq!(
            branch_info_snapshot(&branch_info_lines(current_thread_id, &thread)),
            "This conversation is not currently in a branch.\ncurrent: 019db89d-d85a-7901-bd8a-fef676f33457"
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
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[1],
                depth: None,
                role: BranchNavigatorRole::Ancestor,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[2],
                depth: Some(2),
                role: BranchNavigatorRole::Current,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[3],
                depth: None,
                role: BranchNavigatorRole::Child,
                anchor_summary: None,
                updated_at: None,
            },
            BranchNavigatorRow {
                thread_id: ids[4],
                depth: Some(42),
                role: BranchNavigatorRole::Child,
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
    fn branch_anchor_label_uses_root_for_main() {
        assert_eq!(
            branch_anchor_label(BranchNavigatorRole::Main, Some("ignored")),
            "root"
        );
    }

    #[test]
    fn branch_anchor_label_uses_anchor_summary() {
        assert_eq!(
            branch_anchor_label(BranchNavigatorRole::Child, Some("your case")),
            "\"...your case\""
        );
    }

    #[test]
    fn branch_anchor_label_falls_back_to_selected_point() {
        assert_eq!(
            branch_anchor_label(BranchNavigatorRole::Child, None),
            "\"selected point\""
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

    #[test]
    fn branch_navigator_action_marks_ancestor_navigation_as_return() {
        let thread_id =
            ThreadId::from_string("019db89d-d85a-7901-bd8a-fef676f33457").expect("thread id");
        let params = branch_navigator_selection_params(
            vec![BranchNavigatorRow {
                thread_id,
                depth: Some(0),
                role: BranchNavigatorRole::Main,
                anchor_summary: None,
                updated_at: None,
            }],
            /*current_row_index*/ 0,
            1_000_000,
        );
        let (tx_raw, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        let tx = AppEventSender::new(tx_raw);
        (params.items[0].actions[0])(&tx);

        assert!(matches!(
            rx.try_recv().expect("branch navigator selection event"),
            AppEvent::SelectBranchNavigatorThread {
                thread_id: selected_thread_id,
                kind: BranchNavigatorSelectionKind::ReturnToAncestor,
            } if selected_thread_id == thread_id
        ));
    }
}
