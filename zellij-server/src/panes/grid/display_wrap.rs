//! Fork-local display wrapping. Terminal storage and wrapped scrolling stay unchanged;
//! this view translates logical display lines to the existing buffer coordinates.

use super::{Grid, Row};
use crate::panes::Selection;
use std::ops::Range;
use zellij_utils::position::Position;

impl Grid {
    pub(super) fn uses_unwrapped_display(&self) -> bool {
        !self.display_wrap_enabled && self.alternate_screen_state.is_none()
    }

    pub(super) fn display_wrap_projection(&self) -> DisplayWrapProjection<'_> {
        DisplayWrapProjection::new(self)
    }

    /// Move by logical display lines, preserving buffer-anchored selections.
    /// Returns false when the normal wrapped scrolling path should be used.
    pub(super) fn scroll_unwrapped_lines(&mut self, count: usize, up: bool) -> bool {
        if !self.uses_unwrapped_display() {
            return false;
        }
        let projection = self.display_wrap_projection();
        let first = projection.first_visible;
        let max_first = projection.lines.len().saturating_sub(self.height);
        let target = if up {
            first.saturating_sub(count)
        } else {
            first.saturating_add(count).min(max_first)
        };
        let target_end = (target + self.height).min(projection.lines.len());
        let source_end = target_end
            .checked_sub(1)
            .map(|line| projection.lines[line].end)
            .unwrap_or(0);
        let viewport_end = self.lines_above.len() + self.viewport.len();
        // The bottom logical line may have hidden continuations in lines_below.
        // On reaching the bottom, consume those too so pending PTY data resumes.
        let range = if up {
            source_end.min(viewport_end)..viewport_end
        } else {
            viewport_end..source_end.max(viewport_end)
        };
        let steps = if count == 0 || (up && target == first) {
            0
        } else {
            projection.wrapped_height(range, self.width)
        };
        let line_count = projection.lines.len();
        let mut selection = self.selection.map_positions(|p| projection.buffer_to_logical(p));
        let flash = self.command_output_flash.map(|selection| {
            selection.map_positions(|p| projection.buffer_to_logical(p))
        });
        drop(projection);

        for _ in 0..steps {
            if up {
                if self.lines_above.is_empty() {
                    break;
                }
                self.scroll_up_one_line();
            } else {
                if self.lines_below.is_empty() {
                    break;
                }
                self.scroll_down_one_line();
            }
        }

        let projection = self.display_wrap_projection();
        let dropped = line_count.saturating_sub(projection.lines.len()) as isize;
        selection = selection.map_positions(|mut p| {
            p.line.0 -= dropped;
            p
        });
        if selection.is_active() && steps > 0 {
            // The moving endpoint follows the mouse; the starting anchor follows text.
            selection.end.line.0 += projection.first_visible as isize - first as isize + dropped;
        }
        let selection = selection.map_positions(|p| projection.logical_to_buffer(p));
        let flash = flash.map(|selection| {
            selection.map_positions(|mut p| {
                p.line.0 -= dropped;
                projection.logical_to_buffer(p)
            })
        });
        self.selection = selection;
        self.command_output_flash = flash;
        self.output_buffer.update_all_lines();
        true
    }

    /// Start a selection using visible coordinates, but store buffer coordinates.
    pub(super) fn start_unwrapped_selection(&mut self, start: &Position) {
        self.click.record_click(*start);
        let projection = self.display_wrap_projection();
        let source = projection.display_to_buffer(*start);
        let bounds = if self.click.is_double_click() {
            projection.word_at(*start, &self.word_separators)
        } else if self.click.is_triple_click() {
            self.osc133_command_around_position(&source)
                .or_else(|| projection.line_at(*start))
        } else {
            None
        };
        if let Some((start, end)) = bounds {
            self.selection.set_start_and_end_positions(start, end);
        } else if !self.click.is_double_click() && !self.click.is_triple_click() {
            self.selection.start(source);
        }
        self.output_buffer.update_all_lines();
        self.mark_for_rerender();
    }

    /// Extend selection across complete logical lines, including clipped tails.
    pub(super) fn update_unwrapped_selection(&mut self, to: &Position) {
        let projection = self.display_wrap_projection();
        let source = projection.display_to_buffer(*to);
        if self.click.is_double_click() {
            if let Some((start, end)) = projection.word_at(*to, &self.word_separators) {
                self.selection.add_word_to_position(start, end);
            }
        } else if self.click.is_triple_click() {
            // A logical line can span several source rows, so use both boundaries.
            if let Some((start, end)) = projection.line_at(*to) {
                self.selection.add_word_to_position(start, end);
            }
        } else {
            self.selection.to(source);
        }
        self.output_buffer.update_all_lines();
        self.mark_for_rerender();
    }

    /// Translate mouse endpoints only; selections and command markers stay buffer-relative.
    pub(super) fn display_position_to_buffer(&self, position: Position) -> Position {
        if self.uses_unwrapped_display() {
            self.display_wrap_projection().display_to_buffer(position)
        } else {
            position
        }
    }
}

/// A borrowed index over logical lines, including continuations below the viewport.
/// Only requested rows are cloned, not the entire scrollback on every repaint.
pub(super) struct DisplayWrapProjection<'a> {
    sources: Vec<&'a Row>,
    lines: Vec<Range<usize>>,
    source_origins: Vec<Position>,
    lines_above: usize,
    first_visible: usize,
    height: usize,
}

impl<'a> DisplayWrapProjection<'a> {
    fn new(grid: &'a Grid) -> Self {
        let sources: Vec<_> = grid
            .lines_above
            .iter()
            .chain(grid.viewport.iter())
            .chain(grid.lines_below.iter())
            .collect();
        let mut lines: Vec<Range<usize>> = vec![];
        let mut source_origins = vec![];
        let mut column = 0;
        let mut viewport_bottom = 0;
        for (index, row) in sources.iter().enumerate() {
            if row.is_canonical || lines.is_empty() {
                lines.push(index..index);
                column = 0;
            }
            let line = lines.len() - 1;
            lines[line].end = index + 1;
            source_origins.push(position(line as isize, column));
            column += row.width();
            if index < grid.lines_above.len() + grid.viewport.len() {
                viewport_bottom = lines.len();
            }
        }
        Self {
            sources,
            lines,
            source_origins,
            lines_above: grid.lines_above.len(),
            first_visible: viewport_bottom.saturating_sub(grid.height),
            height: grid.height,
        }
    }

    pub(super) fn visible_rows(&self) -> Vec<Row> {
        (self.first_visible..(self.first_visible + self.height).min(self.lines.len()))
            .map(|line| self.row(line))
            .collect()
    }

    pub(super) fn scrollback_position_and_length(&self) -> (usize, usize) {
        let length = self.lines.len().saturating_sub(self.height);
        (length.saturating_sub(self.first_visible), length)
    }

    pub(super) fn cursor_coordinates(&self, x: usize, y: usize) -> Option<(usize, usize)> {
        let cursor = self.buffer_to_display(position(y as isize, x));
        (cursor.line.0 >= 0 && cursor.line.0 < self.height as isize)
            .then_some((cursor.column.0, cursor.line.0 as usize))
    }

    pub(super) fn selection_to_display(&self, selection: Selection) -> Selection {
        selection.map_positions(|p| self.buffer_to_display(p))
    }

    pub(super) fn display_to_buffer(&self, mut p: Position) -> Position {
        p.line.0 += self.first_visible as isize;
        self.logical_to_buffer(p)
    }

    fn buffer_to_display(&self, p: Position) -> Position {
        let mut p = self.buffer_to_logical(p);
        p.line.0 -= self.first_visible as isize;
        p
    }

    fn buffer_to_logical(&self, p: Position) -> Position {
        let index = p.line.0 + self.lines_above as isize;
        if index < 0 {
            return position(index, p.column.0);
        }
        match self.source_origins.get(index as usize) {
            Some(origin) => position(origin.line.0, origin.column.0 + p.column.0),
            None => position(
                self.lines.len() as isize + index - self.sources.len() as isize,
                p.column.0,
            ),
        }
    }

    fn logical_to_buffer(&self, p: Position) -> Position {
        if p.line.0 < 0 {
            return position(p.line.0 - self.lines_above as isize, p.column.0);
        }
        let Some(range) = self.lines.get(p.line.0 as usize) else {
            return position(
                self.sources.len() as isize - self.lines_above as isize + p.line.0
                    - self.lines.len() as isize,
                p.column.0,
            );
        };
        for index in range.clone() {
            let origin = self.source_origins[index].column.0;
            let column = p.column.0.saturating_sub(origin);
            // Bias an exact wrap boundary toward the preceding fragment's end.
            if column <= self.sources[index].width() || index + 1 == range.end {
                return position(index as isize - self.lines_above as isize, column);
            }
        }
        unreachable!("logical lines always have at least one source row")
    }

    /// Extract complete selected text rather than the width-clipped render rows.
    pub(super) fn selected_text(&self, selection: &Selection) -> Option<String> {
        if selection.is_empty() {
            return None;
        }
        let selection = selection
            .map_positions(|p| self.buffer_to_logical(p))
            .sorted();
        let mut selected_lines = vec![];
        let first = selection.start.line.0.max(0) as usize;
        let end = (selection.end.line.0 + 1).max(0) as usize;
        for line in first..end.min(self.lines.len()) {
            let start_column = if line as isize == selection.start.line.0 {
                selection.start.column.0
            } else {
                0
            };
            let end_column = if line as isize == selection.end.line.0 {
                selection.end.column.0
            } else {
                usize::MAX
            };
            if start_column == end_column {
                continue;
            }
            let mut text = String::new();
            let mut column = 0;
            for source in self.lines[line].clone() {
                for character in &self.sources[source].columns {
                    if (start_column..end_column).contains(&column) {
                        text.push(character.character);
                    }
                    column += character.width();
                }
            }
            selected_lines.push(text.trim_end().to_owned());
        }
        (!selected_lines.is_empty()).then(|| selected_lines.join("\n"))
    }

    pub(super) fn word_at(&self, p: Position, separators: &str) -> Option<(Position, Position)> {
        let line = usize::try_from(p.line.0 + self.first_visible as isize).ok()?;
        self.lines.get(line)?;
        let row = self.row(line);
        let (start, end) = row.word_indices_around_character_index(p.column.0, separators)?;
        Some((
            self.logical_to_buffer(position(line as isize, start)),
            self.logical_to_buffer(position(line as isize, end)),
        ))
    }

    pub(super) fn line_at(&self, p: Position) -> Option<(Position, Position)> {
        let line = usize::try_from(p.line.0 + self.first_visible as isize).ok()?;
        let range = self.lines.get(line)?;
        let width = range.clone().map(|index| self.sources[index].width()).sum();
        Some((
            self.logical_to_buffer(position(line as isize, 0)),
            self.logical_to_buffer(position(line as isize, width)),
        ))
    }

    fn row(&self, line: usize) -> Row {
        let range = self.lines[line].clone();
        let mut row = self.sources[range.start].clone().canonical();
        for index in (range.start + 1)..range.end {
            row.append(&mut self.sources[index].clone());
        }
        row
    }

    /// Count physical rows using the same wide-character boundaries as terminal reflow.
    fn wrapped_height(&self, range: Range<usize>, width: usize) -> usize {
        self.sources[range]
            .iter()
            .map(|row| {
                let mut rows = 1;
                let mut column = 0;
                for character in &row.columns {
                    if column + character.width() > width {
                        rows += 1;
                        column = 0;
                    }
                    column += character.width();
                }
                rows
            })
            .sum()
    }
}

fn position(line: isize, column: usize) -> Position {
    let mut position = Position::default();
    position.change_line(line);
    position.change_column(column);
    position
}

#[cfg(test)]
mod tests;
