//! Inline agent-branch marker rendering for replayed parent-thread assistant replies.

use super::*;
use crate::terminal_hyperlinks::remap_wrapped_line;
use crate::terminal_palette::best_color;
use crate::text_formatting::truncate_text;
use ratatui::style::Style;
use ratatui::text::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentBranchMarker {
    pub(crate) source_line_index: usize,
    pub(crate) source_byte_offset: usize,
    pub(crate) branch_depth: u32,
    pub(crate) branch_id_suffix: String,
    pub(crate) selection_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedBranchMarkerInfo {
    pub(crate) branch_depth: u32,
    pub(crate) branch_id_suffix: String,
    pub(crate) selection_summary: String,
}

#[derive(Debug, Clone)]
pub(super) enum AgentDisplayLineKind {
    Message { use_agent_bullet: bool },
    BranchChrome,
}

#[derive(Debug, Clone)]
pub(super) struct AgentDisplayLine {
    pub(super) line: HyperlinkLine,
    pub(super) kind: AgentDisplayLineKind,
}

pub(crate) fn new_unresolved_branch_markers_event(
    markers: Vec<UnresolvedBranchMarkerInfo>,
) -> PlainHistoryCell {
    let block_style = branch_marker_block_style();
    let mut lines = vec![
        vec![
            "• ".dim(),
            "Some child branches could not be placed inline and are shown here:".into(),
        ]
        .into(),
        Line::from(""),
        Line::from("").style(block_style),
    ];

    for marker in markers {
        lines.push(
            Line::from(vec![
                "⎇ ".into(),
                "Branch ".bold(),
                format!("d{} · {}", marker.branch_depth, marker.branch_id_suffix)
                    .cyan()
                    .bold(),
                ": ".into(),
                format!(
                    "\"...{}\"",
                    truncate_text(marker.selection_summary.trim(), /*max_graphemes*/ 48)
                )
                .dim(),
            ])
            .style(block_style),
        );
    }
    lines.push(Line::from("").style(block_style));
    lines.push(Line::from(""));

    PlainHistoryCell::new(lines)
}

pub(super) fn source_lines_plain_text(lines: &[HyperlinkLine]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line_plain_text(&line.line))
        .collect()
}

pub(super) fn add_branch_marker(
    branch_markers: &mut Vec<AgentBranchMarker>,
    marker: AgentBranchMarker,
) {
    branch_markers.push(marker);
    branch_markers.sort_by_key(|marker| (marker.source_line_index, marker.source_byte_offset));
}

pub(super) fn display_source_lines(
    lines: &[HyperlinkLine],
    is_first_line: bool,
    branch_markers: &[AgentBranchMarker],
) -> Vec<AgentDisplayLine> {
    if branch_markers.is_empty() {
        return lines
            .iter()
            .cloned()
            .enumerate()
            .map(|(source_line_index, line)| AgentDisplayLine {
                line,
                kind: AgentDisplayLineKind::Message {
                    use_agent_bullet: source_line_index == 0 && is_first_line,
                },
            })
            .collect();
    }

    let mut markers_by_line: HashMap<usize, Vec<&AgentBranchMarker>> = HashMap::new();
    for marker in branch_markers {
        markers_by_line
            .entry(marker.source_line_index)
            .or_default()
            .push(marker);
    }

    let mut output = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let Some(mut line_markers) = markers_by_line.remove(&line_index) else {
            output.push(AgentDisplayLine {
                line: line.clone(),
                kind: AgentDisplayLineKind::Message {
                    use_agent_bullet: line_index == 0 && is_first_line,
                },
            });
            continue;
        };
        line_markers.sort_by_key(|marker| marker.source_byte_offset);
        output.extend(decorate_line_with_branch_markers(
            line_index,
            is_first_line,
            line,
            line_markers,
        ));
    }

    output
}

fn decorate_line_with_branch_markers(
    source_line_index: usize,
    is_first_line: bool,
    line: &HyperlinkLine,
    markers: Vec<&AgentBranchMarker>,
) -> Vec<AgentDisplayLine> {
    let mut grouped_markers: Vec<(usize, Vec<&AgentBranchMarker>)> = Vec::new();
    for marker in markers {
        if let Some((offset, grouped)) = grouped_markers.last_mut()
            && *offset == marker.source_byte_offset
        {
            grouped.push(marker);
        } else {
            grouped_markers.push((marker.source_byte_offset, vec![marker]));
        }
    }

    let mut output = Vec::new();
    let mut remainder = line.clone();
    let mut consumed_bytes = 0usize;
    for (source_byte_offset, grouped_markers) in grouped_markers {
        let Some(split_at) =
            split_offset_after_anchor(&line_plain_text(&line.line), source_byte_offset)
        else {
            continue;
        };
        let relative_split_at = split_at.saturating_sub(consumed_bytes);
        let (prefix, suffix) = split_hyperlink_line_at_byte_offset(&remainder, relative_split_at);
        if !line_is_empty(&prefix.line) {
            output.push(AgentDisplayLine {
                line: prefix,
                kind: AgentDisplayLineKind::Message {
                    use_agent_bullet: source_line_index == 0
                        && consumed_bytes == 0
                        && is_first_line,
                },
            });
        }
        output.extend(marker_block_lines(&grouped_markers));
        remainder = trim_hyperlink_line_leading_spaces(suffix);
        consumed_bytes = split_at;
    }

    if !line_is_empty(&remainder.line) {
        output.push(AgentDisplayLine {
            line: remainder,
            kind: AgentDisplayLineKind::Message {
                use_agent_bullet: false,
            },
        });
    }
    output
}

fn marker_block_lines(markers: &[&AgentBranchMarker]) -> Vec<AgentDisplayLine> {
    let block_style = branch_marker_block_style();
    let mut lines = vec![
        AgentDisplayLine {
            line: "".into(),
            kind: AgentDisplayLineKind::BranchChrome,
        },
        AgentDisplayLine {
            line: HyperlinkLine::new(Line::from("").style(block_style)),
            kind: AgentDisplayLineKind::BranchChrome,
        },
    ];
    for marker in markers {
        let summary = truncate_text(marker.selection_summary.trim(), /*max_graphemes*/ 48);
        lines.push(AgentDisplayLine {
            line: HyperlinkLine::new(
                Line::from(vec![
                    "⎇ ".into(),
                    "Branch ".bold(),
                    format!("d{} · {}", marker.branch_depth, marker.branch_id_suffix)
                        .cyan()
                        .bold(),
                    ": ".into(),
                    format!("\"...{summary}\"").dim(),
                ])
                .style(block_style),
            ),
            kind: AgentDisplayLineKind::BranchChrome,
        });
    }
    lines.push(AgentDisplayLine {
        line: HyperlinkLine::new(Line::from("").style(block_style)),
        kind: AgentDisplayLineKind::BranchChrome,
    });
    lines.push(AgentDisplayLine {
        line: "".into(),
        kind: AgentDisplayLineKind::BranchChrome,
    });
    lines
}

fn line_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn split_offset_after_anchor(text: &str, source_byte_offset: usize) -> Option<usize> {
    text.char_indices()
        .find_map(|(byte_index, ch)| {
            let end = byte_index + ch.len_utf8();
            (source_byte_offset < end).then_some(end)
        })
        .or_else(|| (source_byte_offset >= text.len()).then_some(text.len()))
}

fn split_hyperlink_line_at_byte_offset(
    line: &HyperlinkLine,
    split_at: usize,
) -> (HyperlinkLine, HyperlinkLine) {
    let (left, right) = split_line_at_byte_offset(&line.line, split_at);
    let left = remap_wrapped_line(line, vec![left])
        .into_iter()
        .next()
        .unwrap_or_default();
    let right = remap_wrapped_line(line, vec![right])
        .into_iter()
        .next()
        .unwrap_or_default();
    (left, right)
}

fn split_line_at_byte_offset(
    line: &Line<'static>,
    split_at: usize,
) -> (Line<'static>, Line<'static>) {
    let mut left_spans = Vec::new();
    let mut right_spans = Vec::new();
    let mut consumed = 0usize;

    for span in &line.spans {
        let span_text = span.content.as_ref();
        let span_len = span_text.len();
        if split_at <= consumed {
            right_spans.push(span.clone());
        } else if split_at >= consumed + span_len {
            left_spans.push(span.clone());
        } else {
            let split_in_span = split_at - consumed;
            let split_in_span = next_char_boundary(span_text, split_in_span);
            let left_text = &span_text[..split_in_span];
            let right_text = &span_text[split_in_span..];
            if !left_text.is_empty() {
                left_spans.push(Span::from(left_text.to_string()).set_style(span.style));
            }
            if !right_text.is_empty() {
                right_spans.push(Span::from(right_text.to_string()).set_style(span.style));
            }
        }
        consumed += span_len;
    }

    (
        Line::from(left_spans).style(line.style),
        Line::from(right_spans).style(line.style),
    )
}

fn trim_hyperlink_line_leading_spaces(line: HyperlinkLine) -> HyperlinkLine {
    let mut spans = Vec::new();
    let mut trimming = true;

    for span in line.line.spans {
        if !trimming {
            spans.push(span);
            continue;
        }

        let trimmed = span.content.trim_start_matches(' ');
        if trimmed.is_empty() {
            continue;
        }

        trimming = false;
        if trimmed.len() == span.content.len() {
            spans.push(span);
        } else {
            spans.push(Span::from(trimmed.to_string()).set_style(span.style));
        }
    }

    HyperlinkLine::new(Line::from(spans).style(line.line.style))
}

fn line_is_empty(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.as_ref().is_empty())
}

fn next_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut offset = byte_offset.min(text.len());
    while offset < text.len() && !text.is_char_boundary(offset) {
        offset = offset.saturating_add(1);
    }
    offset
}

fn branch_marker_block_style() -> Style {
    Style::new().bg(best_color((19, 53, 74)))
}
