use super::TerminalPane;
use crate::panes::{kitty_graphics::KittyImageStore, sixel::SixelImageStore, LinkHandler};
use crate::tab::Pane;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use zellij_utils::{
    data::{Palette, Style},
    pane_size::PaneGeom,
};

#[test]
fn reset_terminal_recovers_modes_and_clears_both_screens() {
    let mut pane = new_pane();
    pane.handle_pty_bytes(b"history\r\n".repeat(30));
    assert!(pane.grid.scrollback_position_and_length().1 > 0);
    pane.handle_pty_bytes(
        b"\x1b[?1049h\x1b[?1h\x1b[?1002;1006h\x1b[?1004h\x1b[?2004h\x1b[?2031h\x1b[?25l\x1b[4h\x1b[?7l\x1b[?2026hbroken".to_vec(),
    );
    assert!(pane.grid.is_alternate_mode_active());
    assert!(pane.grid.lock_renders);
    assert!(pane.grid.bracketed_paste_mode);
    pane.reset_terminal();
    assert!(!pane.grid.is_alternate_mode_active());
    assert!(!pane.grid.lock_renders);
    assert!(!pane.grid.cursor_key_mode);
    assert!(!pane.grid.bracketed_paste_mode);
    assert!(!pane.grid.focus_event_tracking);
    assert!(!pane.grid.color_palette_notification_enabled);
    assert!(!pane.grid.insert_mode);
    assert!(!pane.grid.disable_linewrap);
    assert!(!pane.grid.supports_kitty_keyboard_protocol);
    assert!(pane.cursor_coordinates().unwrap().2);
    assert_eq!(pane.grid.dump_screen(true), "");
    assert!(pane.should_render());
    // A late alternate-screen exit cannot resurrect discarded history.
    pane.handle_pty_bytes(b"\x1b[?1049lrecovered".to_vec());
    assert_eq!(pane.grid.dump_screen(true), "recovered");
}

#[test]
fn reset_terminal_aborts_unfinished_escape_sequences() {
    for sequence in [
        &b"\x1b[31"[..],
        &b"\x1b]2;unfinished title"[..],
        &b"\x1bP+q544e"[..],
        &b"\x1bPzunfinished UI"[..],
        &b"\x1b_Ga=T,f=24;AAAA"[..],
    ] {
        let mut pane = new_pane();
        pane.handle_pty_bytes(sequence.to_vec());
        pane.reset_terminal();
        pane.handle_pty_bytes(b"recovered".to_vec());
        assert_eq!(pane.grid.dump_screen(true), "recovered", "{sequence:?}");
    }
}

#[test]
fn reset_terminal_discards_paused_output_without_changing_pane_identity() {
    let mut pane = new_pane();
    pane.update_name("keep me");
    let geom = pane.geom;
    pane.forward_paused = true;
    pane.handle_pty_bytes(b"\x1b[?1049hstale".to_vec());
    pane.grid.pending_messages_to_pty.push(b"stale reply".to_vec());
    pane.reset_terminal();
    assert!(!pane.forward_paused);
    assert!(pane.pending_pty_input.is_empty());
    assert!(pane.grid.pending_messages_to_pty.is_empty());
    assert_eq!(pane.pid, 1);
    assert_eq!(pane.geom, geom);
    assert_eq!(pane.pane_name, "keep me");
    assert!(!pane.is_held());
    pane.handle_pty_bytes(b"recovered".to_vec());
    assert_eq!(pane.grid.dump_screen(true), "recovered");
}

fn new_pane() -> TerminalPane {
    let mut geom = PaneGeom::default();
    geom.cols.set_inner(80);
    geom.rows.set_inner(10);
    TerminalPane::new(
        1,
        geom,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}
