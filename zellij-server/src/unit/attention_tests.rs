use super::*;
use crate::panes::terminal_character::AnsiCode;
use crate::ui::pane_boundaries_frame::{ATTENTION_BACKGROUND, ATTENTION_FOREGROUND};

#[test]
fn explicit_notifications_request_attention_without_visual_or_host_bells() {
    for signal in [
        "\x1b]9;Build finished\x1b\\",
        "\x1b]9;Build finished\x07",
        "\x1b]99;;Build finished\x1b\\",
        "\x1b]777;notify;Build;Finished\x1b\\",
    ] {
        let (mut screen, server_receiver) = attention_screen();
        send_attention_output(&mut screen, 0, 2, signal);
        let tab = &screen.tabs[&0];
        assert!(tab.get_pane_with_id(PaneId::Terminal(2)).unwrap().has_attention());
        assert!(tab.has_pending_attention());
        assert!(tab.panes_with_pending_bell.is_empty());
        assert!(!tab.tab_has_pending_bell);
        let output = collect_forwarded_notifications(&server_receiver);
        assert!(!output.contains('\x07'));
        assert!(!output.contains("\x1b]9;"));
        assert!(!output.contains("\x1b]99;"));

        send_attention_output(&mut screen, 0, 2, signal);
        assert!(screen.tabs[&0].has_pending_attention());
        screen.tabs.get_mut(&0).unwrap().move_focus_down(1).unwrap();
        screen.acknowledge_focused_pane(1);
        assert!(!screen.tabs[&0].has_pending_attention());
    }
}

#[test]
fn attention_ignores_output_bells_and_notification_control_traffic() {
    let (mut screen, _) = attention_screen();
    for signal in [
        "ordinary output",
        "\x07",
        "\x1b]9;4;1;50\x1b\\", // ConEmu progress, not a notification
        "\x1b]99;i=build:p=?;\x1b\\",
        "\x1b]99;i=build:p=alive;\x1b\\",
        "\x1b]99;i=build:p=close;\x1b\\",
        "\x1b]99;i=build:d=0;Build\x1b\\",
    ] {
        send_attention_output(&mut screen, 0, 2, signal);
        assert!(!screen.tabs[&0].has_pending_attention(), "{signal:?}");
    }
    send_attention_output(&mut screen, 0, 2, "\x1b]99;i=build; finished\x1b\\");
    assert!(screen.tabs[&0].has_pending_attention());
}

#[test]
fn attention_in_a_background_window_clears_when_the_window_is_focused() {
    let (mut screen, _) = attention_screen();
    send_attention_output(&mut screen, 0, 1, "\x1b]9;Already looking\x1b\\");
    assert!(!screen.tabs[&0].has_pending_attention());

    screen.host_terminal_focus_changed(1, false);
    send_attention_output(&mut screen, 0, 1, "\x1b]9;Come back\x1b\\");
    assert!(screen.tabs[&0].has_pending_attention());
    screen.acknowledge_focused_pane(1);
    assert!(screen.tabs[&0].has_pending_attention());

    screen.host_terminal_focus_changed(1, true);
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn visiting_a_tab_only_acknowledges_its_selected_pane() {
    let (mut screen, _) = attention_screen();
    new_tab(&mut screen, 3, 1);
    for pane in [1, 2] {
        send_attention_output(&mut screen, 0, pane, "\x1b]9;Finished\x1b\\");
    }
    screen.switch_active_tab(0, None, true, 1).unwrap();
    let tab = &screen.tabs[&0];
    assert!(!tab.get_pane_with_id(PaneId::Terminal(1)).unwrap().has_attention());
    assert!(tab.get_pane_with_id(PaneId::Terminal(2)).unwrap().has_attention());
    assert!(tab.has_pending_attention());

    screen.switch_active_tab(1, None, true, 1).unwrap();
    screen.switch_active_tab(0, None, true, 1).unwrap();
    assert!(screen.tabs[&0].has_pending_attention());
    screen.tabs.get_mut(&0).unwrap().move_focus_down(1).unwrap();
    screen.acknowledge_focused_pane(1);
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn mouse_focus_acknowledges_attention_without_a_click() {
    let (mut screen, _) = attention_screen();
    screen.tabs.get_mut(&0).unwrap().update_focus_follows_mouse(true);
    send_attention_output(&mut screen, 0, 2, "\x1b]9;Finished\x1b\\");
    assert!(screen.tabs[&0].has_pending_attention());

    screen.handle_mouse_event(MouseEvent::new_buttonless_motion(Position::new(15, 60)), 1);

    assert_eq!(screen.get_active_pane_id(&1), Some(PaneId::Terminal(2)));
    assert!(!screen.tabs[&0].get_pane_with_id(PaneId::Terminal(2)).unwrap().has_attention());
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn hovering_without_focus_leaves_attention_pending_until_a_click() {
    let (mut screen, _) = attention_screen();
    screen.tabs.get_mut(&0).unwrap().update_focus_follows_mouse(false);
    send_attention_output(&mut screen, 0, 2, "\x1b]9;Finished\x1b\\");
    let position = Position::new(15, 60);

    screen.handle_mouse_event(MouseEvent::new_buttonless_motion(position), 1);

    assert_eq!(screen.get_active_pane_id(&1), Some(PaneId::Terminal(1)));
    assert!(screen.tabs[&0].has_pending_attention());
    screen.handle_mouse_event(MouseEvent::new_left_press_event(position), 1);
    assert_eq!(screen.get_active_pane_id(&1), Some(PaneId::Terminal(2)));
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn mouse_focus_only_acknowledges_the_newly_focused_pane() {
    let (mut screen, _) = attention_screen();
    screen.tabs.get_mut(&0).unwrap().update_focus_follows_mouse(true);
    send_attention_output(&mut screen, 0, 2, "\x1b]9;Finished\x1b\\");
    screen.tabs.get_mut(&0).unwrap().set_pane_attention(PaneId::Terminal(1), true);

    screen.handle_mouse_event(MouseEvent::new_buttonless_motion(Position::new(15, 60)), 1);

    assert_eq!(screen.get_active_pane_id(&1), Some(PaneId::Terminal(2)));
    assert!(!screen.tabs[&0].get_pane_with_id(PaneId::Terminal(2)).unwrap().has_attention());
    assert!(screen.tabs[&0].get_pane_with_id(PaneId::Terminal(1)).unwrap().has_attention());
    assert!(screen.tabs[&0].has_pending_attention());

    screen.handle_mouse_event(MouseEvent::new_buttonless_motion(Position::new(5, 60)), 1);

    assert_eq!(screen.get_active_pane_id(&1), Some(PaneId::Terminal(1)));
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn attention_disappears_with_the_pane() {
    let (mut screen, _) = attention_screen();
    send_attention_output(&mut screen, 0, 2, "\x1b]9;Finished\x1b\\");
    screen.tabs.get_mut(&0).unwrap().close_pane(PaneId::Terminal(2), false, None);
    assert!(!screen.tabs[&0].has_pending_attention());
}

#[test]
fn tab_attention_recolors_only_the_label_without_changing_geometry() {
    let (mut screen, _) = attention_screen();
    screen.tabs.get_mut(&0).unwrap().name = "shiny".to_owned();
    new_tab(&mut screen, 3, 1);
    screen.tabs.get_mut(&1).unwrap().name = "z".to_owned();
    for active_tab in [0, 1] {
        for cols in [1, 5, 9, 40] {
            screen.tabs.get_mut(&0).unwrap().set_pane_attention(PaneId::Terminal(2), false);
            let segments_before = screen.acme_tab_bar_segments(cols, Some(active_tab));
            let normal = screen.acme_tab_bar_chunk(cols, Some(active_tab), None, Some(InputMode::Normal));
            screen.tabs.get_mut(&0).unwrap().set_pane_attention(PaneId::Terminal(2), true);
            let highlighted = screen.acme_tab_bar_chunk(cols, Some(active_tab), None, Some(InputMode::Normal));
            assert_eq!(segments_before, screen.acme_tab_bar_segments(cols, Some(active_tab)));
            assert_eq!(normal.terminal_characters.len(), highlighted.terminal_characters.len());
            for (column, (before, after)) in normal.terminal_characters.iter()
                .zip(&highlighted.terminal_characters).enumerate() {
                assert_eq!(before.character, after.character);
                assert_eq!(before.width(), after.width());
                assert_eq!(before.styles.bold, after.styles.bold);
                assert_eq!(before.styles.underline, after.styles.underline);
                if cols > 1 && column < 9 {
                    assert_eq!(after.styles.background, Some(ATTENTION_BACKGROUND));
                    assert_eq!(after.styles.foreground, Some(ATTENTION_FOREGROUND));
                } else {
                    assert_eq!(before.styles, after.styles);
                }
            }
        }
    }
    let active = screen.acme_tab_bar_chunk(40, Some(0), None, Some(InputMode::Tmux));
    assert_eq!(active.terminal_characters[1].character, '■');
    assert_eq!(active.terminal_characters[3].styles.bold, Some(AnsiCode::On));
    assert_eq!(active.terminal_characters[3].styles.background, Some(ATTENTION_BACKGROUND));
}

/// Build a quiet two-pane tab with pane 1 focused and pane 2 below it.
fn attention_screen() -> (Screen, ServerReceiver) {
    let (mut screen, server_receiver) = screen_with_a_client_for_notifications(
        HostNotificationProtocol::Off, BTreeMap::new(),
    );
    screen.visual_bell = false;
    let tab = screen.tabs.get_mut(&0).unwrap();
    tab.horizontal_split(PaneId::Terminal(2), None, 1, None, None).unwrap();
    tab.move_focus_up(1).unwrap();
    (screen, server_receiver)
}

/// Exercise the pane parser and screen notification handler without running the screen thread.
fn send_attention_output(screen: &mut Screen, tab_id: usize, terminal_id: u32, output: &str) {
    let pane = screen.tabs.get_mut(&tab_id).unwrap()
        .get_pane_with_id_mut(PaneId::Terminal(terminal_id)).unwrap();
    pane.handle_pty_bytes(output.as_bytes().to_vec());
    let notifications = pane.drain_desktop_notifications();
    screen.handle_desktop_notifications(notifications, terminal_id);
}
