use super::TerminalPane;
use crate::panes::{kitty_graphics::KittyImageStore, sixel::SixelImageStore, LinkHandler};
use crate::tab::{AdjustedInput, Pane};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use zellij_utils::{
    data::{BareKey, KeyWithModifier, Palette, Style},
    input::command::RunCommand,
    pane_size::PaneGeom,
};

#[test]
fn startup_choices_preserve_restored_history_and_reset_terminal_modes() {
    for key in [BareKey::Enter, BareKey::Esc] {
        let mut pane = restored_pane();
        pane.handle_pty_bytes(b"\x1b[?1h\x1b[?1004h\x1b[?25l".to_vec());
        let action = press_key(&mut pane, key);
        match key {
            BareKey::Enter => assert!(matches!(
                action,
                Some(AdjustedInput::RunCommandInShellInThisPane(_))
            )),
            BareKey::Esc => assert!(matches!(
                action,
                Some(AdjustedInput::DropToShellInThisPane { .. })
            )),
            _ => unreachable!(),
        }
        assert!(!pane.grid.cursor_key_mode);
        assert!(!pane.grid.focus_event_tracking);
        assert!(pane.cursor_coordinates().unwrap().2);
        assert!(pane.initial_contents.is_none());
        assert!(pane.banner.is_none());
        assert!(!pane.is_held());
        assert_restored_history(&pane);

        pane.handle_pty_bytes(b"NEW OUTPUT\r\n".to_vec());
        assert_restored_history(&pane);
        assert!(pane.grid.dump_screen(false).contains("NEW OUTPUT"));
    }
}

#[test]
fn programmatic_rerun_preserves_restored_history_once() {
    let mut pane = restored_pane();
    assert!(pane.rerun().is_some());
    assert_restored_history(&pane);

    pane.hold(Some(0), false, RunCommand::default());
    assert!(pane.rerun().is_some());
    assert!(!pane.grid.dump_screen(true).contains("history"));
    assert!(pane.initial_contents.is_none());
}

#[test]
fn banner_redraws_and_saves_do_not_overwrite_restored_contents() {
    let mut pane = restored_pane();
    let saved_contents = pane.serialize(Some(0)).unwrap();
    for columns in [60, 100, 80] {
        let mut geom = pane.geom;
        geom.cols.set_inner(columns);
        pane.set_geom(geom);
        pane.update_theme(Style::default().colors);
        assert!(pane.grid.dump_screen(false).contains("Waiting to run:"));
        assert_eq!(pane.serialize(Some(0)).unwrap(), saved_contents);
        assert!(pane.grid.dump_screen(true).contains("history 00"));
    }
    press_key(&mut pane, BareKey::Esc);
    assert_restored_history(&pane);
}

#[test]
fn explicit_terminal_reset_after_startup_still_clears_history() {
    let mut pane = restored_pane();
    press_key(&mut pane, BareKey::Enter);
    assert_restored_history(&pane);
    pane.handle_pty_bytes(b"\x1bc".to_vec());
    assert!(!pane.grid.dump_screen(true).contains("history"));
    assert!(!pane.serialize(Some(0)).unwrap().contains("history"));
}

#[test]
fn panes_without_saved_contents_keep_existing_startup_behavior() {
    for first_run in [false, true] {
        for key in [BareKey::Enter, BareKey::Esc] {
            let mut pane = new_pane();
            pane.handle_pty_bytes(b"PREVIOUS OUTPUT\r\n".to_vec());
            pane.hold(Some(0), first_run, RunCommand::default());
            assert!(press_key(&mut pane, key).is_some());
            assert!(!pane.grid.dump_screen(true).contains("PREVIOUS OUTPUT"));
            assert!(!pane.grid.dump_screen(true).contains("Waiting to run:"));
        }
    }
}

#[test]
fn unheld_initial_contents_are_not_retained_for_later_replay() {
    let mut pane = new_pane();
    pane.restore_initial_contents("INITIAL OUTPUT", false);
    assert!(pane.grid.dump_screen(false).contains("INITIAL OUTPUT"));
    assert!(pane.initial_contents.is_none());
    pane.handle_pty_bytes(b"\x1bcLIVE OUTPUT".to_vec());
    let saved_contents = pane.serialize(Some(0)).unwrap();
    assert!(saved_contents.contains("LIVE OUTPUT"));
    assert!(!saved_contents.contains("INITIAL OUTPUT"));
}

#[test]
fn serialize_osc133_restores_history_with_one_fresh_prompt_after_either_startup_choice() {
    for key in [BareKey::Enter, BareKey::Esc] {
        let mut source = new_pane();
        source.handle_pty_bytes(
            b"\x1b]133;A\x07old$ \x1b]133;B\x07echo hi\x1b]133;C\x07\r\noutput\x1b]133;D;0\x07\r\n\x1b]133;A\x07prompt$ \x1b]133;B\x07".to_vec(),
        );
        let saved_contents = source.serialize(Some(0)).unwrap();
        assert!(source.grid.dump_screen(true).contains("prompt$"));

        let mut pane = new_pane();
        pane.restore_initial_contents(&saved_contents, true);
        pane.hold(None, true, RunCommand::default());
        assert!(press_key(&mut pane, key).is_some());
        pane.handle_pty_bytes(b"prompt$ ".to_vec());
        let contents = pane.grid.dump_screen(true);
        assert!(contents.contains("old$ echo hi"));
        assert!(contents.contains("output"));
        assert_eq!(contents.matches("prompt$").count(), 1);
    }
}

fn press_key(pane: &mut TerminalPane, key: BareKey) -> Option<AdjustedInput> {
    pane.adjust_input_to_terminal(&Some(KeyWithModifier::new(key)), vec![], false, Some(1))
}

fn assert_restored_history(pane: &TerminalPane) {
    let contents = pane.grid.dump_screen(true);
    for line in 0..40 {
        let marker = format!("history {line:02}");
        assert_eq!(contents.matches(&marker).count(), 1, "{marker}: {contents}");
    }
    assert!(!contents.contains("Waiting to run:"));
    assert!(!contents.contains("drop to shell"));
    assert!(pane.grid.scrollback_position_and_length().1 > 0);
}

fn restored_pane() -> TerminalPane {
    let mut source = new_pane();
    for line in 0..40 {
        source.handle_pty_bytes(format!("\x1b[31mhistory {line:02}\x1b[0m\r\n").into_bytes());
    }
    let contents = source.serialize(Some(0)).unwrap();
    let mut pane = new_pane();
    pane.restore_initial_contents(&contents, true);
    pane.hold(None, true, RunCommand::default());
    pane
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
