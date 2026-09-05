use super::*;
use crate::panes::grid::SixelImageStore;
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::link_handler::LinkHandler;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use zellij_utils::data::{Palette, Style};

#[test]
fn scrolling_counts_logical_lines_and_stops_at_both_ends() {
    let lines = long_lines(8);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    let live = rendered_lines(&mut grid);
    assert_eq!(live, clipped(&lines[4..]));
    assert_eq!(grid.scrollback_position_and_length(), (0, 4));

    grid.move_viewport_up(1);
    assert_eq!(rendered_lines(&mut grid), clipped(&lines[3..7]));
    assert_eq!(grid.scrollback_position_and_length(), (1, 4));
    assert!(grid.is_scrolled);

    grid.move_viewport_up(100);
    assert_eq!(rendered_lines(&mut grid), clipped(&lines[..4]));
    assert_eq!(grid.scrollback_position_and_length(), (4, 4));
    for _ in 0..5 {
        grid.move_viewport_up(1);
        assert_eq!(rendered_lines(&mut grid), clipped(&lines[..4]));
    }

    grid.move_viewport_down(1);
    assert_eq!(rendered_lines(&mut grid), clipped(&lines[1..5]));
    grid.move_viewport_down(100);
    assert_eq!(rendered_lines(&mut grid), live);
    assert_eq!(grid.scrollback_position_and_length(), (0, 4));
    assert!(!grid.is_scrolled);
    assert!(grid.lines_below.is_empty());
    grid.move_viewport_down(1);
    assert_eq!(rendered_lines(&mut grid), live);
}

#[test]
fn no_scroll_when_all_logical_lines_fit_even_if_wrapped_scrollback_exists() {
    let lines = long_lines(3);
    let mut grid = grid(3, 8, &lines.join("\r\n"));
    assert!(!grid.lines_above.is_empty());
    let before = rendered_lines(&mut grid);
    grid.move_viewport_up(100);
    assert_eq!(rendered_lines(&mut grid), before);
    assert_eq!(grid.scrollback_position_and_length(), (0, 0));
    assert!(!grid.is_scrolled);
}

#[test]
fn drag_copies_hidden_tails_but_preserves_partial_endpoints() {
    let lines = long_lines(8);
    for (start, end) in [(position(0, 3), position(2, 5)), (position(2, 5), position(0, 3))] {
        let mut grid = grid(4, 8, &lines.join("\r\n"));
        select(&mut grid, start, end);
        assert_eq!(
            grid.get_selected_text().unwrap(),
            format!("{}\n{}\n{}", &lines[4][3..], lines[5], &lines[6][..5])
        );
        let (chunks, _, _, _) = grid.render(2, 3, &Style::default()).unwrap().unwrap();
        let selected = |row: usize, column: usize| chunks[row].selection_and_colors()
            .iter().any(|highlight| highlight.selection.contains(row + 3, column + 2));
        assert!(!selected(0, 2));
        assert!(selected(0, 3));
        assert!(selected(0, 7));
        assert!(selected(1, 7));
        assert!(selected(2, 4));
        assert!(!selected(2, 5));
        assert!(!selected(3, 0));
    }
}

#[test]
fn same_line_drag_only_copies_the_selected_columns() {
    let lines = long_lines(8);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    select(&mut grid, position(0, 3), position(0, 6));
    assert!(grid.selection.start.line.0 < 0, "displayed text comes from scrollback");
    assert_eq!(grid.get_selected_text().unwrap(), "abc");
}

#[test]
fn selection_remains_anchored_after_scrolling_in_both_directions() {
    let lines = long_lines(9);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    select(&mut grid, position(0, 3), position(2, 5));
    let selected = grid.get_selected_text();
    grid.move_viewport_up(2);
    assert_eq!(grid.get_selected_text(), selected);
    grid.move_viewport_down(1);
    assert_eq!(grid.get_selected_text(), selected);
    grid.move_viewport_down(100);
    assert_eq!(grid.get_selected_text(), selected);
}

#[test]
fn active_drag_scrolls_by_logical_lines_and_keeps_mouse_endpoint() {
    let lines = long_lines(8);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    grid.start_selection(&position(1, 3));
    grid.update_selection(&position(0, 2));
    grid.move_viewport_up(1);
    grid.end_selection(&position(0, 2));
    assert_eq!(grid.get_selected_text().unwrap(),
        format!("{}\n{}\n{}", &lines[3][2..], lines[4], &lines[5][..3]));
}

#[test]
fn word_and_line_selection_include_hidden_text() {
    for clicks in [2, 3] {
        let mut grid = grid(3, 8, "abcdefghijklmnop\r\nqrstuvwxyzabcdef\r\nlast");
        for _ in 0..clicks {
            grid.start_selection(&position(0, 3));
        }
        grid.end_selection(&position(0, 3));
        assert_eq!(grid.get_selected_text().unwrap(), "abcdefghijklmnop");
    }
}

#[test]
fn triple_click_drag_selects_complete_logical_lines() {
    let lines = long_lines(6);
    let mut grid = grid(3, 8, &lines.join("\r\n"));
    for _ in 0..3 {
        grid.start_selection(&position(0, 3));
    }
    grid.update_selection(&position(2, 4));
    grid.end_selection(&position(2, 4));
    assert_eq!(grid.get_selected_text().unwrap(), lines[3..].join("\n"));
}

#[test]
fn hidden_continuations_below_the_wrapped_viewport_are_copied() {
    let lines = long_lines(8);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    // Raw scroll is also used by search; stop in the middle of a logical line.
    grid.scroll_up_one_line();
    assert!(!grid.lines_below.front().unwrap().is_canonical);
    for _ in 0..3 {
        grid.start_selection(&position(3, 1));
    }
    grid.end_selection(&position(3, 1));
    assert_eq!(grid.get_selected_text().unwrap(), lines[7]);
    grid.move_viewport_down(1);
    assert!(!grid.is_scrolled);
    assert!(grid.lines_below.is_empty());
}

#[test]
fn wide_characters_use_cell_columns_and_scroll_at_reflow_boundaries() {
    let lines: Vec<_> = (0..8).map(|i| format!("{i}界界界界界界界界")).collect();
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    select(&mut grid, position(0, 3), position(2, 3));
    assert_eq!(grid.get_selected_text().unwrap(),
        format!("{}\n{}\n6界", "界".repeat(7), lines[5]));
    grid.move_viewport_up(1);
    let rendered = rendered_lines(&mut grid);
    assert!(rendered[0].starts_with('3'));
    assert!(rendered[3].starts_with('6'));
    grid.move_viewport_down(1);
    assert!(!grid.is_scrolled);
}

#[test]
fn toggling_and_resize_keep_terminal_wrapping_intact() {
    let lines = long_lines(8);
    let mut grid = grid(4, 8, &lines.join("\r\n"));
    let stored = grid.dump_screen(true);
    let original = rendered_lines(&mut grid);
    grid.move_viewport_up(2);
    grid.toggle_display_wrap();
    assert!(!grid.is_scrolled);
    assert_eq!(grid.dump_screen(true), stored);
    grid.toggle_display_wrap();
    assert_eq!(rendered_lines(&mut grid), original);
    grid.change_size(5, 10);
    grid.move_viewport_up(100);
    let rendered = rendered_lines(&mut grid);
    assert_eq!(rendered.len(), 5);
    assert!(rendered[0].starts_with("00-"));
    grid.move_viewport_down(100);
    assert!(!grid.is_scrolled);
}

#[test]
fn alternate_screen_ignores_display_wrap_setting() {
    let mut grid = grid(3, 8, "before");
    vte::Parser::new().advance(&mut grid, b"\x1b[?1049habcdefghijklmnop");
    assert!(!grid.uses_unwrapped_display());
    assert_eq!(rendered_lines(&mut grid)[..2], ["abcdefgh", "ijklmnop"]);
    select(&mut grid, position(0, 2), position(1, 3));
    assert_eq!(grid.get_selected_text().unwrap(), "cdefghijk");
}

#[test]
fn empty_rows_and_newline_endpoints_preserve_normal_selection() {
    let mut grid = grid(4, 8, "abcdefghijklmnop\r\n\r\nqrstuvwxyz\r\nlast");
    select(&mut grid, position(0, 2), position(2, 0));
    assert_eq!(grid.get_selected_text().unwrap(), "cdefghijklmnop\n");
}

fn select(grid: &mut Grid, start: Position, end: Position) {
    grid.start_selection(&start);
    grid.update_selection(&end);
    grid.end_selection(&end);
}

fn long_lines(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{i:02}-abcdefghijklmno")).collect()
}

fn clipped(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| line[..8].to_owned()).collect()
}

fn rendered_lines(grid: &mut Grid) -> Vec<String> {
    grid.render_full_viewport();
    let (chunks, _, _, _) = grid.render(0, 0, &Style::default()).unwrap().unwrap();
    chunks.iter().map(|chunk| chunk.terminal_characters.iter()
        .map(|character| character.character).collect()).collect()
}

fn grid(height: usize, width: usize, content: &str) -> Grid {
    let mut grid = Grid::new(
        height,
        width,
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Style::default(),
        false,
        true,
        true,
        true,
        false,
    );
    vte::Parser::new().advance(&mut grid, content.as_bytes());
    grid.toggle_display_wrap();
    grid
}
