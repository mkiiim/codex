use std::sync::Arc;

use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::pager_overlay::TranscriptForkAnchor;
use crate::pager_overlay::TranscriptReadPosition;
use crate::pager_overlay::TranscriptReplyTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BranchSnippetError {
    EmptySnippet,
    NoAssistantResponse,
    NoMatch,
    AmbiguousMatch,
    NoAnchor,
}

pub(crate) fn locate_branch_snippet(
    cells: &[Arc<dyn HistoryCell>],
    raw_assistant_markdown: Option<&str>,
    snippet: &str,
    width: u16,
) -> Result<TranscriptReplyTarget, BranchSnippetError> {
    let snippets = normalized_snippet_candidates(snippet);
    if snippets
        .iter()
        .all(|snippet| snippet.text.trim().is_empty())
    {
        return Err(BranchSnippetError::EmptySnippet);
    }

    let Some(response) = latest_assistant_response(cells) else {
        return Err(BranchSnippetError::NoAssistantResponse);
    };

    let source_lines = response.source_lines();
    let normalized_source = NormalizedText::new(&source_lines);
    let match_range = find_branch_snippet_match(&normalized_source.text, &snippets)?;
    let (match_end_line_index, _) = normalized_source
        .mapping
        .get(match_range.end.saturating_sub(1))
        .copied()
        .ok_or(BranchSnippetError::NoAnchor)?;

    let source_line_index = structure_end_line(&source_lines, match_end_line_index);
    let (cell_index, cell, cell_source_line_index) = response
        .cell_position(source_line_index)
        .ok_or(BranchSnippetError::NoAnchor)?;
    let source_byte_offset = cell
        .source_line_end_anchor(cell_source_line_index)
        .ok_or(BranchSnippetError::NoAnchor)?;
    let read_position = TranscriptReadPosition {
        cell_index,
        source_line_index: cell_source_line_index,
        source_byte_offset,
    };
    let assistant_positions = assistant_positions(cells, width);
    let current_index = assistant_positions
        .iter()
        .position(|position| *position == read_position)
        .map_or_else(
            || {
                assistant_positions
                    .iter()
                    .take_while(|position| {
                        (
                            position.cell_index,
                            position.source_line_index,
                            position.source_byte_offset,
                        ) <= (
                            read_position.cell_index,
                            read_position.source_line_index,
                            read_position.source_byte_offset,
                        )
                    })
                    .count()
            },
            |index| index + 1,
        );

    Ok(TranscriptReplyTarget {
        read_position,
        fork_anchor: raw_assistant_markdown
            .and_then(|markdown| response.fork_anchor(markdown, &snippets)),
        anchor_summary: branch_anchor_summary(&source_lines, source_line_index, source_byte_offset),
        current_index,
        total: assistant_positions.len(),
    })
}

struct AssistantResponse<'a> {
    cells: Vec<(usize, &'a AgentMessageCell)>,
}

impl AssistantResponse<'_> {
    fn source_lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .flat_map(|(_, cell)| cell.source_lines_plain_text())
            .collect()
    }

    fn cell_position(
        &self,
        response_line_index: usize,
    ) -> Option<(usize, &AgentMessageCell, usize)> {
        let mut remaining = response_line_index;
        for (cell_index, cell) in &self.cells {
            let line_count = cell.source_lines_plain_text().len();
            if remaining < line_count {
                return Some((*cell_index, cell, remaining));
            }
            remaining = remaining.saturating_sub(line_count);
        }
        None
    }

    fn fork_anchor(
        &self,
        raw_assistant_markdown: &str,
        snippets: &[NormalizedText],
    ) -> Option<TranscriptForkAnchor> {
        let raw_lines = raw_assistant_markdown
            .split('\n')
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let normalized_raw = NormalizedText::new(&raw_lines);
        let match_range = find_branch_snippet_match(&normalized_raw.text, snippets).ok()?;
        let (match_end_line_index, _) = normalized_raw
            .mapping
            .get(match_range.end.saturating_sub(1))
            .copied()?;
        let source_line_index = structure_end_line(&raw_lines, match_end_line_index);
        let source_byte_offset =
            absolute_line_end_anchor(raw_assistant_markdown, &raw_lines, source_line_index)?;

        Some(TranscriptForkAnchor::LatestAssistant {
            source_line_index: 0,
            source_byte_offset,
        })
    }
}

fn latest_assistant_response(cells: &[Arc<dyn HistoryCell>]) -> Option<AssistantResponse<'_>> {
    let (last_index, _) = cells.iter().enumerate().rev().find_map(|(index, cell)| {
        cell.as_any()
            .downcast_ref::<AgentMessageCell>()
            .map(|cell| (index, cell))
    })?;

    let mut first_index = last_index;
    while first_index > 0 {
        let Some(current_cell) = cells[first_index]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
        else {
            break;
        };
        if !current_cell.is_stream_continuation() {
            break;
        }
        if cells[first_index - 1]
            .as_any()
            .downcast_ref::<AgentMessageCell>()
            .is_none()
        {
            break;
        }
        first_index -= 1;
    }

    Some(AssistantResponse {
        cells: (first_index..=last_index)
            .filter_map(|cell_index| {
                cells[cell_index]
                    .as_any()
                    .downcast_ref::<AgentMessageCell>()
                    .map(|cell| (cell_index, cell))
            })
            .collect(),
    })
}

fn absolute_line_end_anchor(
    raw_text: &str,
    raw_lines: &[String],
    line_index: usize,
) -> Option<usize> {
    let line_start = raw_lines
        .iter()
        .take(line_index)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let line = raw_lines.get(line_index)?;
    let line_anchor = AgentMessageCell::transcript_anchor_byte_offset(line, &(0..line.len()))?;
    let absolute_anchor = line_start + line_anchor;
    (absolute_anchor < raw_text.len()).then_some(absolute_anchor)
}

fn normalized_snippet_candidates(snippet: &str) -> Vec<NormalizedText> {
    let raw_lines = vec![snippet.to_string()];
    let stripped_lines = snippet
        .lines()
        .map(strip_main_view_assistant_chrome)
        .collect::<Vec<_>>();
    let raw = NormalizedText::new(&raw_lines);
    let stripped = NormalizedText::new(&stripped_lines);
    if raw.text == stripped.text {
        vec![raw]
    } else {
        vec![raw, stripped]
    }
}

fn strip_main_view_assistant_chrome(line: &str) -> String {
    let trimmed = line.trim_start_matches([' ', '\t']);
    trimmed
        .strip_prefix('•')
        .map_or_else(|| line.to_string(), |rest| rest.trim_start().to_string())
}

fn find_branch_snippet_match(
    source: &str,
    snippets: &[NormalizedText],
) -> Result<std::ops::Range<usize>, BranchSnippetError> {
    let mut last_error = BranchSnippetError::NoMatch;
    for snippet in snippets {
        match find_unique_match(source, &snippet.text) {
            Ok(range) => return Ok(range),
            Err(BranchSnippetError::NoMatch) => {}
            Err(err) => last_error = err,
        }
    }
    Err(last_error)
}

fn find_unique_match(
    source: &str,
    snippet: &str,
) -> Result<std::ops::Range<usize>, BranchSnippetError> {
    let Some(start) = source.find(snippet) else {
        return Err(BranchSnippetError::NoMatch);
    };
    let end = start + snippet.len();
    if source[end..].contains(snippet) {
        return Err(BranchSnippetError::AmbiguousMatch);
    }
    Ok(start..end)
}

fn structure_end_line(source_lines: &[String], line_index: usize) -> usize {
    if let Some(list_item_start) = containing_list_item_start(source_lines, line_index) {
        return source_lines
            .iter()
            .enumerate()
            .skip(list_item_start)
            .take_while(|(index, line)| {
                *index == list_item_start || (!line.trim().is_empty() && !is_list_item_start(line))
            })
            .map(|(index, _)| index)
            .last()
            .unwrap_or(line_index);
    }

    source_lines
        .iter()
        .enumerate()
        .skip(line_index)
        .take_while(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .last()
        .unwrap_or(line_index)
}

fn branch_anchor_summary(
    source_lines: &[String],
    source_line_index: usize,
    source_byte_offset: usize,
) -> Option<String> {
    const SUMMARY_WORDS: usize = 6;

    let line = source_lines.get(source_line_index)?;
    let end = next_char_boundary(line, source_byte_offset.min(line.len()));
    let summary = line[..end]
        .split_whitespace()
        .rev()
        .take(SUMMARY_WORDS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");
    (!summary.is_empty()).then_some(summary)
}

fn previous_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut offset = byte_offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn next_char_boundary(text: &str, byte_offset: usize) -> usize {
    let offset = previous_char_boundary(text, byte_offset);
    if offset >= text.len() {
        return offset;
    }
    text[offset..]
        .chars()
        .next()
        .map_or(offset, |ch| offset + ch.len_utf8())
}

fn containing_list_item_start(source_lines: &[String], line_index: usize) -> Option<usize> {
    source_lines
        .iter()
        .enumerate()
        .take(line_index + 1)
        .rev()
        .take_while(|(_, line)| !line.trim().is_empty())
        .find_map(|(index, line)| is_list_item_start(line).then_some(index))
}

fn is_list_item_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }

    let Some((marker, rest)) = trimmed.split_once(['.', ')']) else {
        return false;
    };
    !rest.is_empty()
        && rest.starts_with(char::is_whitespace)
        && marker.chars().all(|ch| ch.is_ascii_digit())
}

fn assistant_positions(cells: &[Arc<dyn HistoryCell>], width: u16) -> Vec<TranscriptReadPosition> {
    cells
        .iter()
        .enumerate()
        .filter_map(|(cell_index, cell)| {
            cell.as_any()
                .downcast_ref::<AgentMessageCell>()
                .map(|cell| (cell_index, cell))
        })
        .flat_map(|(cell_index, cell)| {
            cell.transcript_line_anchors(width).into_iter().map(
                move |(source_line_index, source_byte_offset)| TranscriptReadPosition {
                    cell_index,
                    source_line_index,
                    source_byte_offset,
                },
            )
        })
        .collect()
}

struct NormalizedText {
    text: String,
    mapping: Vec<(usize, usize)>,
}

impl NormalizedText {
    fn new(lines: &[String]) -> Self {
        let mut text = String::new();
        let mut mapping = Vec::new();
        let mut pending_space: Option<(usize, usize)> = None;

        for (line_index, line) in lines.iter().enumerate() {
            for (byte_offset, ch) in line.char_indices() {
                if ch.is_whitespace() {
                    pending_space.get_or_insert((line_index, byte_offset));
                } else {
                    if !text.is_empty()
                        && let Some((space_line_index, space_byte_offset)) = pending_space.take()
                    {
                        text.push(' ');
                        mapping.push((space_line_index, space_byte_offset));
                    }
                    text.push(ch);
                    mapping.push((line_index, byte_offset));
                    pending_space = None;
                }
            }
            pending_space.get_or_insert((line_index, line.len()));
        }

        Self { text, mapping }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;

    use super::*;

    fn agent_cell(lines: &[&str]) -> Arc<dyn HistoryCell> {
        agent_cell_with_first_line(lines, true)
    }

    fn agent_continuation_cell(lines: &[&str]) -> Arc<dyn HistoryCell> {
        agent_cell_with_first_line(lines, false)
    }

    fn agent_cell_with_first_line(lines: &[&str], is_first_line: bool) -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            lines
                .iter()
                .map(|line| Line::from((*line).to_string()))
                .collect(),
            is_first_line,
        ))
    }

    #[test]
    fn locates_wrapped_snippet_and_expands_to_paragraph_end() {
        let cells = vec![agent_cell(&[
            "First paragraph starts here",
            "and continues to the sentence end.",
            "",
            "Unread paragraph.",
        ])];

        let target = locate_branch_snippet(
            &cells,
            /*raw_assistant_markdown*/ None,
            "starts here and continues",
            80,
        )
        .expect("snippet should match");

        assert_eq!(
            target.read_position,
            TranscriptReadPosition {
                cell_index: 0,
                source_line_index: 1,
                source_byte_offset: "and continues to the sentence end.".len() - 1,
            }
        );
    }

    #[test]
    fn tolerates_main_view_assistant_bullet_chrome() {
        let cells = vec![agent_cell(&["First assistant line", "continues here."])];

        let target = locate_branch_snippet(
            &cells,
            /*raw_assistant_markdown*/ None,
            "• First assistant line continues",
            80,
        )
        .expect("snippet should match after stripping assistant chrome");

        assert_eq!(
            target.read_position,
            TranscriptReadPosition {
                cell_index: 0,
                source_line_index: 1,
                source_byte_offset: "continues here.".len() - 1,
            }
        );
    }

    #[test]
    fn reports_ambiguous_snippet_in_latest_assistant_response() {
        let cells = vec![agent_cell(&["repeat here", "repeat there"])];

        let result =
            locate_branch_snippet(&cells, /*raw_assistant_markdown*/ None, "repeat", 80);

        assert_eq!(result, Err(BranchSnippetError::AmbiguousMatch));
    }

    #[test]
    fn searches_only_latest_assistant_response() {
        let cells = vec![
            agent_cell(&["older response has unique text"]),
            agent_cell(&["newer response"]),
        ];

        let result = locate_branch_snippet(
            &cells,
            /*raw_assistant_markdown*/ None,
            "unique text",
            80,
        );

        assert_eq!(result, Err(BranchSnippetError::NoMatch));
    }

    #[test]
    fn searches_all_chunks_of_latest_streamed_assistant_response() {
        let cells = vec![
            agent_cell(&["Previous assistant response."]),
            agent_cell(&[
                "At a high level, a retainer consulting contract should pin down six things clearly:",
                "",
                "1. Scope and exclusions: exactly what you will do.",
                "2. Fee structure: monthly retainer amount, whether hours roll over, whether the",
                "retainer is earned on receipt or applied against time, overage rates,",
                "invoicing cadence, payment terms, and late-fee rules.",
            ]),
            agent_continuation_cell(&[
                "3. Capacity and service levels: how much availability the client is buying.",
                "4. Term and exit rules: start date, renewal, minimum term.",
            ]),
        ];

        let target = locate_branch_snippet(
            &cells,
            Some(
                "At a high level, a retainer consulting contract should pin down six things clearly:\n\n1. Scope and exclusions: exactly what you will do.\n2. Fee structure: monthly retainer amount, whether hours roll over, whether the\nretainer is earned on receipt or applied against time, overage rates,\ninvoicing cadence, payment terms, and late-fee rules.\n3. Capacity and service levels: how much availability the client is buying.\n4. Term and exit rules: start date, renewal, minimum term.",
            ),
            "2. Fee structure: monthly retainer amount, whether hours roll over, whether the\n     retainer is earned on receipt or applied against time",
            80,
        )
        .expect("snippet should match an earlier chunk of the latest response");

        assert_eq!(
            target.read_position,
            TranscriptReadPosition {
                cell_index: 1,
                source_line_index: 5,
                source_byte_offset: "invoicing cadence, payment terms, and late-fee rules.".len()
                    - 1,
            }
        );
        assert_eq!(
            target.fork_anchor,
            Some(TranscriptForkAnchor::LatestAssistant {
                source_line_index: 0,
                source_byte_offset: 338,
            })
        );
        assert_eq!(
            target.anchor_summary,
            Some("cadence, payment terms, and late-fee rules.".to_string())
        );
    }
}
