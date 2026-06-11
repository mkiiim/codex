//! Parent-thread child-branch marker preparation and attachment during replay.

use super::*;
use crate::branch_chrome::branch_chrome_debug_enabled;
use crate::history_cell::AgentMessageCell;
use crate::markdown::append_markdown;
use codex_app_server_protocol::ThreadForkSnapshot;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadSourceKind;

impl App {
    pub(super) async fn prepare_direct_child_branch_markers(
        &mut self,
        app_server: &mut AppServerSession,
        parent_thread_id: ThreadId,
    ) {
        self.next_assistant_history_cell_index = 0;
        self.current_assistant_message_source_line_offset = 0;
        self.current_assistant_message_source_byte_offset = 0;
        match self
            .direct_child_branch_markers_for_thread(app_server, parent_thread_id)
            .await
        {
            Ok(markers) => {
                self.pending_direct_child_branch_markers = markers;
            }
            Err(err) => {
                self.pending_direct_child_branch_markers.clear();
                tracing::warn!(
                    parent_thread_id = %parent_thread_id,
                    error = %err,
                    "failed to load direct child branch markers"
                );
            }
        }
    }

    pub(super) fn flush_unresolved_direct_child_branch_markers(&mut self) {
        if self.pending_direct_child_branch_markers.is_empty() {
            return;
        }

        let branch_debug = branch_chrome_debug_enabled();
        if branch_debug {
            for marker in &mut self.pending_direct_child_branch_markers {
                if marker.unresolved_reason.is_none() {
                    marker.unresolved_reason = Some(
                        PendingDirectChildBranchMarkerUnresolvedReason::NeverMatchedAssistantMessageCell,
                    );
                }
                tracing::info!(
                    branch_id = %marker.branch_id_suffix,
                    anchor_kind = ?marker.anchor_kind,
                    assistant_message_index = marker.assistant_message_index,
                    source_line_index = marker.source_line_index,
                    source_byte_offset = marker.source_byte_offset,
                    reason = %pending_direct_child_branch_marker_unresolved_reason_text(
                        marker.unresolved_reason.unwrap_or(
                            PendingDirectChildBranchMarkerUnresolvedReason::NeverMatchedAssistantMessageCell,
                        ),
                    ),
                    "branch marker unresolved"
                );
            }
        }

        let unresolved = self
            .pending_direct_child_branch_markers
            .drain(..)
            .map(|marker| history_cell::UnresolvedBranchMarkerInfo {
                branch_depth: marker.branch_depth,
                branch_id_suffix: marker.branch_id_suffix,
                selection_summary: marker.selection_summary,
            })
            .collect::<Vec<_>>();
        self.chat_widget
            .add_to_history(history_cell::new_unresolved_branch_markers_event(
                unresolved,
            ));
    }

    async fn direct_child_branch_markers_for_thread(
        &mut self,
        app_server: &mut AppServerSession,
        parent_thread_id: ThreadId,
    ) -> Result<Vec<PendingDirectChildBranchMarker>> {
        let parent_thread_id_text = parent_thread_id.to_string();
        let mut cursor = None;
        let mut child_threads = Vec::new();
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

            for thread in response.data {
                if thread.forked_from_id.as_deref() != Some(parent_thread_id_text.as_str()) {
                    continue;
                }
                child_threads.push(thread);
            }

            cursor = response.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        let latest_assistant_message_index = if child_threads.iter().any(|thread| {
            matches!(
                thread.branch_origin_snapshot,
                Some(ThreadForkSnapshot::LatestAssistantReadAnchor { .. })
            )
        }) {
            latest_assistant_message_index_for_thread(app_server, parent_thread_id).await
        } else {
            None
        };

        let mut markers = Vec::new();
        for thread in child_threads {
            if let Some(marker) =
                pending_direct_child_branch_marker(thread, latest_assistant_message_index)
            {
                markers.push(marker);
            }
        }

        for marker in &mut markers {
            marker.branch_id_suffix = marker.branch_id_suffix.chars().rev().collect();
        }
        markers.sort_by_key(|marker| {
            (
                marker.assistant_message_index,
                marker.source_line_index,
                marker.source_byte_offset,
                marker.branch_id_suffix.clone(),
            )
        });
        Ok(markers)
    }

    pub(super) fn decorate_inserted_agent_history_cell(&mut self, cell: &mut dyn HistoryCell) {
        let Some(agent_cell) = cell.as_any_mut().downcast_mut::<AgentMessageCell>() else {
            return;
        };

        let source_lines = agent_cell.source_lines_plain_text();
        let line_count = source_lines.len();
        let current_index = if agent_cell.is_stream_continuation() {
            self.next_assistant_history_cell_index.saturating_sub(1)
        } else {
            self.current_assistant_message_source_line_offset = 0;
            self.current_assistant_message_source_byte_offset = 0;
            let current_index = self.next_assistant_history_cell_index;
            self.next_assistant_history_cell_index =
                self.next_assistant_history_cell_index.saturating_add(1);
            current_index
        };
        let current_line_offset = self.current_assistant_message_source_line_offset;
        let current_line_end = current_line_offset.saturating_add(line_count);
        let mut matched_markers = Vec::new();
        let branch_debug = branch_chrome_debug_enabled();
        self.pending_direct_child_branch_markers.retain_mut(|marker| {
            if marker.assistant_message_index != current_index {
                return true;
            }

            let Some(raw_markdown) = agent_cell.raw_markdown_text() else {
                marker.unresolved_reason = Some(
                    PendingDirectChildBranchMarkerUnresolvedReason::MissingRawMarkdown,
                );
                if branch_debug {
                    tracing::info!(
                        branch_id = %marker.branch_id_suffix,
                        assistant_message_index = marker.assistant_message_index,
                        "branch marker unresolved: missing raw markdown"
                    );
                }
                return true;
            };
            let Some((local_source_line_index, display_source_byte_offset)) =
                display_source_position_for_marker(
                    raw_markdown,
                    &source_lines,
                    agent_cell.source_segments(),
                    marker,
                    Some(self.config.cwd.as_path()),
                )
            else {
                marker.unresolved_reason = Some(
                    PendingDirectChildBranchMarkerUnresolvedReason::SourcePositionCouldNotBeMapped,
                );
                if branch_debug {
                    tracing::info!(
                        branch_id = %marker.branch_id_suffix,
                        assistant_message_index = marker.assistant_message_index,
                        source_line_index = marker.source_line_index,
                        source_byte_offset = marker.source_byte_offset,
                        source_lines = source_lines.len(),
                        "branch marker unresolved: source position could not be mapped"
                    );
                }
                return true;
            };
            let source_line_index = current_line_offset.saturating_add(local_source_line_index);
            if source_line_index < current_line_offset || source_line_index >= current_line_end {
                marker.unresolved_reason = Some(
                    PendingDirectChildBranchMarkerUnresolvedReason::MappedSourceLineOutsideCurrentCell,
                );
                if branch_debug {
                    tracing::info!(
                        branch_id = %marker.branch_id_suffix,
                        assistant_message_index = marker.assistant_message_index,
                        local_source_line_index,
                        current_line_offset,
                        current_line_end,
                        "branch marker unresolved: mapped source line fell outside current cell"
                    );
                }
                return true;
            }
            let display_source_byte_offset = snap_branch_marker_display_offset(
                &source_lines[local_source_line_index],
                display_source_byte_offset,
            );

            let mut marker = marker.clone();
            marker.source_line_index = local_source_line_index;
            marker.source_byte_offset = display_source_byte_offset;
            matched_markers.push(marker);
            false
        });

        for marker in matched_markers {
            agent_cell.add_branch_marker(history_cell::AgentBranchMarker {
                source_line_index: marker.source_line_index,
                source_byte_offset: marker.source_byte_offset,
                branch_depth: marker.branch_depth,
                branch_id_suffix: marker.branch_id_suffix,
                selection_summary: marker.selection_summary,
            });
        }

        self.current_assistant_message_source_line_offset = self
            .current_assistant_message_source_line_offset
            .saturating_add(line_count);
        self.current_assistant_message_source_byte_offset = self
            .current_assistant_message_source_byte_offset
            .saturating_add(agent_cell.raw_markdown_text().map_or(0, str::len));
    }
}

fn pending_direct_child_branch_marker(
    thread: codex_app_server_protocol::Thread,
    latest_assistant_message_index: Option<usize>,
) -> Option<PendingDirectChildBranchMarker> {
    let (anchor_kind, assistant_message_index, source_line_index, source_byte_offset) =
        match thread.branch_origin_snapshot? {
            ThreadForkSnapshot::AssistantReadAnchor {
                assistant_message_index,
                source_line_index,
                source_byte_offset,
            } => (
                PendingDirectChildBranchMarkerAnchor::AssistantRead,
                assistant_message_index,
                source_line_index,
                source_byte_offset,
            ),
            ThreadForkSnapshot::LatestAssistantReadAnchor {
                source_line_index,
                source_byte_offset,
            } => (
                PendingDirectChildBranchMarkerAnchor::LatestAssistantRead,
                u32::try_from(latest_assistant_message_index?).ok()?,
                source_line_index,
                source_byte_offset,
            ),
            ThreadForkSnapshot::Interrupted => return None,
        };

    Some(PendingDirectChildBranchMarker {
        anchor_kind,
        assistant_message_index: usize::try_from(assistant_message_index).unwrap_or(usize::MAX),
        source_line_index: usize::try_from(source_line_index).unwrap_or(usize::MAX),
        source_byte_offset: usize::try_from(source_byte_offset).unwrap_or(usize::MAX),
        branch_depth: thread.branch_depth.unwrap_or(0),
        branch_id_suffix: thread.id.chars().rev().take(4).collect::<String>(),
        selection_summary: thread
            .branch_anchor_summary
            .or(thread.branch_anchor_head_summary)
            .or(thread.branch_anchor_tail_summary)
            .unwrap_or_else(|| "branch".to_string()),
        unresolved_reason: None,
    })
}

fn pending_direct_child_branch_marker_unresolved_reason_text(
    reason: PendingDirectChildBranchMarkerUnresolvedReason,
) -> &'static str {
    match reason {
        PendingDirectChildBranchMarkerUnresolvedReason::MissingRawMarkdown => {
            "missing_raw_markdown"
        }
        PendingDirectChildBranchMarkerUnresolvedReason::SourcePositionCouldNotBeMapped => {
            "source_position_could_not_be_mapped"
        }
        PendingDirectChildBranchMarkerUnresolvedReason::MappedSourceLineOutsideCurrentCell => {
            "mapped_source_line_outside_current_cell"
        }
        PendingDirectChildBranchMarkerUnresolvedReason::NeverMatchedAssistantMessageCell => {
            "never_matched_assistant_message_cell"
        }
    }
}

async fn latest_assistant_message_index_for_thread(
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
) -> Option<usize> {
    let thread = app_server
        .thread_read(thread_id, /*include_turns*/ true)
        .await
        .ok()?;
    let assistant_message_count = thread
        .turns
        .iter()
        .flat_map(|turn| turn.items.iter())
        .filter(|item| matches!(item, ThreadItem::AgentMessage { .. }))
        .count();
    assistant_message_count.checked_sub(1)
}

fn display_source_position_for_marker(
    raw_chunk: &str,
    source_lines: &[String],
    source_segments: Option<&[String]>,
    marker: &PendingDirectChildBranchMarker,
    cwd: Option<&std::path::Path>,
) -> Option<(usize, usize)> {
    let absolute_byte_offset = absolute_marker_byte_offset(source_segments, marker)?;
    if let Some((raw_source_line_index, raw_source_byte_offset)) =
        raw_line_position_for_absolute_byte_offset(raw_chunk, absolute_byte_offset)
        && let Some(raw_line) = raw_chunk.split('\n').nth(raw_source_line_index)
        && let Some(local_source_line_index) =
            display_line_index_for_raw_line(source_lines, raw_line)
        && let Some(display_source_byte_offset) = display_byte_offset_for_raw_line_anchor(
            raw_line,
            &source_lines[local_source_line_index],
            raw_source_byte_offset,
            cwd,
        )
    {
        return Some((local_source_line_index, display_source_byte_offset));
    }

    let exclusive_byte_offset = exclusive_marker_byte_offset(raw_chunk, absolute_byte_offset);
    let mut rendered_prefix_lines = Vec::new();
    append_markdown(
        &raw_chunk[..exclusive_byte_offset],
        /*width*/ None,
        cwd,
        &mut rendered_prefix_lines,
    );

    let rendered_prefix_lines = rendered_prefix_lines
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    rendered_prefix_lines.last()?;
    for candidate_source_line_index in (0..source_lines.len()).rev() {
        let suffix_len = candidate_source_line_index + 1;
        let Some(rendered_suffix_start) = rendered_prefix_lines.len().checked_sub(suffix_len)
        else {
            continue;
        };
        let rendered_prefix_line = &rendered_prefix_lines[rendered_prefix_lines.len() - 1];
        let source_line = &source_lines[candidate_source_line_index];
        if !source_line.starts_with(rendered_prefix_line) {
            continue;
        }
        let preceding_lines_match = source_lines[..candidate_source_line_index]
            .iter()
            .zip(
                rendered_prefix_lines[rendered_suffix_start..rendered_prefix_lines.len() - 1]
                    .iter(),
            )
            .all(|(source_line, rendered_prefix_line)| source_line == rendered_prefix_line);
        if preceding_lines_match {
            return Some((candidate_source_line_index, rendered_prefix_line.len()));
        }
    }

    None
}

fn absolute_marker_byte_offset(
    source_segments: Option<&[String]>,
    marker: &PendingDirectChildBranchMarker,
) -> Option<usize> {
    let Some(source_segments) = source_segments else {
        return (marker.source_line_index == 0).then_some(marker.source_byte_offset);
    };

    let segment_index = marker.source_line_index;
    let segment = source_segments.get(segment_index)?;
    if marker.source_byte_offset > segment.len() {
        return None;
    }

    Some(
        source_segments
            .iter()
            .take(segment_index)
            .map(String::len)
            .sum::<usize>()
            + marker.source_byte_offset,
    )
}

fn exclusive_marker_byte_offset(raw_chunk: &str, absolute_byte_offset: usize) -> usize {
    if absolute_byte_offset >= raw_chunk.len() {
        return raw_chunk.len();
    }

    if raw_chunk[absolute_byte_offset..].starts_with('\n') {
        return absolute_byte_offset;
    }

    next_char_boundary(raw_chunk, absolute_byte_offset.saturating_add(1))
}

fn raw_line_position_for_absolute_byte_offset(
    raw_chunk: &str,
    absolute_byte_offset: usize,
) -> Option<(usize, usize)> {
    let mut current_offset = 0usize;
    for (line_index, line) in raw_chunk.split('\n').enumerate() {
        let line_end = current_offset + line.len();
        if absolute_byte_offset <= line_end {
            return Some((
                line_index,
                absolute_byte_offset.saturating_sub(current_offset),
            ));
        }
        current_offset = line_end.saturating_add(1);
    }

    None
}

fn display_line_index_for_raw_line(source_lines: &[String], raw_line: &str) -> Option<usize> {
    let normalized_raw_line = normalize_marker_source_line(raw_line);
    source_lines
        .iter()
        .position(|line| normalize_marker_source_line(line) == normalized_raw_line)
}

fn normalize_marker_source_line(line: &str) -> String {
    line.replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .trim()
        .to_string()
}

fn display_byte_offset_for_raw_line_anchor(
    raw_line: &str,
    display_line: &str,
    raw_source_byte_offset: usize,
    cwd: Option<&std::path::Path>,
) -> Option<usize> {
    let raw_prefix_end = next_char_boundary(raw_line, raw_source_byte_offset.min(raw_line.len()));
    let mut rendered_prefix_lines = Vec::new();
    append_markdown(
        &raw_line[..raw_prefix_end],
        /*width*/ None,
        cwd,
        &mut rendered_prefix_lines,
    );
    let rendered_prefix = rendered_prefix_lines
        .last()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .unwrap_or_default();
    display_line
        .starts_with(rendered_prefix.as_str())
        .then_some(rendered_prefix.len())
}

fn snap_branch_marker_display_offset(display_line: &str, display_byte_offset: usize) -> usize {
    let bounded_offset =
        next_char_boundary(display_line, display_byte_offset.min(display_line.len()));
    if bounded_offset > 0
        && display_line[..bounded_offset]
            .chars()
            .next_back()
            .is_some_and(|ch| matches!(ch, '.' | '!' | '?'))
        && (bounded_offset == display_line.len()
            || display_line[bounded_offset..].starts_with(char::is_whitespace))
    {
        return bounded_offset;
    }
    let remaining = &display_line[bounded_offset..];
    let mut saw_word = false;

    for (relative_index, ch) in remaining.char_indices() {
        if !saw_word {
            if ch.is_whitespace() {
                continue;
            }
            saw_word = true;
        }

        if matches!(ch, '.' | '!' | '?') {
            let punctuation_end = relative_index + ch.len_utf8();
            let trailing = &remaining[punctuation_end..];
            let trailing_punctuation_len = trailing
                .chars()
                .take_while(|next| matches!(next, '.' | '!' | '?'))
                .map(char::len_utf8)
                .sum::<usize>();
            return bounded_offset + punctuation_end + trailing_punctuation_len;
        }
    }

    display_line.len()
}

fn next_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut offset = byte_offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset = offset.saturating_add(1);
    }
    offset
}
