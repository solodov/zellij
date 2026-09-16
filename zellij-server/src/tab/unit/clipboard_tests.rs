use super::{tab_tests::create_new_tab_with_copy_options, Tab};
use crate::{plugins::PluginInstruction, screen::CopyOptions, ServerInstruction};
use std::path::Path;
use std::time::{Duration, Instant};
use zellij_utils::channels::{unbounded, ChannelWithContext, Receiver, SenderWithContext};
use zellij_utils::data::{CopyDestination, Event};
use zellij_utils::errors::ErrorContext;
use zellij_utils::input::{mouse::MouseEvent, options::PaneFrameStyle};
use zellij_utils::pane_size::Size;
use zellij_utils::position::Position;

#[test]
fn mouse_copy_uses_override_but_explicit_and_plugin_copies_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("explicit");
    let automatic = dir.path().join("automatic");
    let options = CopyOptions {
        command: Some(copy_command(dir.path(), &explicit)),
        copy_on_select_command: Some(copy_command(dir.path(), &automatic)),
        ..CopyOptions::default()
    };
    let (mut tab, _) = new_tab(options);

    select_text_with_mouse(&mut tab);
    wait_for_copy(&automatic, "selected text");
    assert!(!explicit.exists());

    tab.copy_selection(1).unwrap();
    wait_for_copy(&explicit, "selected text");

    tab.copy_text_to_clipboard("plugin copy").unwrap();
    wait_for_copy(&explicit, "plugin copy");
    assert_eq!(std::fs::read_to_string(automatic).unwrap(), "selected text");
}

#[test]
fn mouse_copy_falls_back_to_copy_command_when_override_is_unset() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("explicit");
    let (mut tab, _) = new_tab(CopyOptions {
        command: Some(copy_command(dir.path(), &explicit)),
        ..CopyOptions::default()
    });
    select_text_with_mouse(&mut tab);
    wait_for_copy(&explicit, "selected text");
}

#[test]
fn mouse_copy_falls_back_to_osc52_and_reports_the_actual_destination() {
    let dir = tempfile::tempdir().unwrap();
    let automatic = dir.path().join("automatic");
    let (mut tab, plugin_receiver) = new_tab(CopyOptions {
        copy_on_select_command: Some(copy_command(dir.path(), &automatic)),
        ..CopyOptions::default()
    });
    let (to_server, server_receiver): ChannelWithContext<ServerInstruction> = unbounded();
    tab.senders.to_server = Some(SenderWithContext::new(to_server));

    select_text_with_mouse(&mut tab);
    wait_for_copy(&automatic, "selected text");
    assert!(
        copy_events(&plugin_receiver).contains(&Event::CopyToClipboard(CopyDestination::Command))
    );
    assert!(!has_osc52(&server_receiver));

    tab.copy_selection(1).unwrap();
    assert!(
        copy_events(&plugin_receiver).contains(&Event::CopyToClipboard(CopyDestination::System))
    );
    assert!(has_osc52(&server_receiver));

    // Removing the override during reload restores OSC52 for automatic copies too.
    tab.update_copy_options(&CopyOptions::default());
    tab.write_selection_to_clipboard_on_select("after reload")
        .unwrap();
    assert!(
        copy_events(&plugin_receiver).contains(&Event::CopyToClipboard(CopyDestination::System))
    );
    assert!(has_osc52(&server_receiver));
}

#[test]
fn reloading_copy_options_replaces_and_removes_the_mouse_command() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    let explicit = dir.path().join("explicit");
    let mut options = CopyOptions {
        command: Some(copy_command(dir.path(), &explicit)),
        copy_on_select_command: Some(copy_command(dir.path(), &first)),
        ..CopyOptions::default()
    };
    let (mut tab, _) = new_tab(options.clone());
    tab.write_selection_to_clipboard_on_select("first").unwrap();
    wait_for_copy(&first, "first");

    options.copy_on_select_command = Some(copy_command(dir.path(), &second));
    tab.update_copy_options(&options);
    tab.write_selection_to_clipboard_on_select("second")
        .unwrap();
    wait_for_copy(&second, "second");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
    assert!(!explicit.exists());

    options.copy_on_select_command = None;
    tab.update_copy_options(&options);
    tab.write_selection_to_clipboard_on_select("fallback")
        .unwrap();
    wait_for_copy(&explicit, "fallback");
    assert_eq!(std::fs::read_to_string(second).unwrap(), "second");
}

#[test]
fn disabling_copy_on_select_preserves_selection_for_explicit_copy() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("explicit");
    let automatic = dir.path().join("automatic");
    let (mut tab, plugin_receiver) = new_tab(CopyOptions {
        command: Some(copy_command(dir.path(), &explicit)),
        copy_on_select_command: Some(copy_command(dir.path(), &automatic)),
        copy_on_select: false,
        ..CopyOptions::default()
    });

    // With no selection, Copy should not invoke either command.
    tab.copy_selection(1).unwrap();
    assert!(copy_events(&plugin_receiver).is_empty());
    assert!(!explicit.exists());
    select_text_with_mouse(&mut tab);
    assert!(copy_events(&plugin_receiver).is_empty());
    assert!(!automatic.exists());
    assert!(!explicit.exists());

    tab.copy_selection(1).unwrap();
    wait_for_copy(&explicit, "selected text");
    assert!(!automatic.exists());
}

#[test]
fn failed_mouse_command_reports_failure_without_falling_back() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("explicit");
    let (mut tab, plugin_receiver) = new_tab(CopyOptions {
        command: Some(copy_command(dir.path(), &explicit)),
        copy_on_select_command: Some(dir.path().join("missing-command").display().to_string()),
        ..CopyOptions::default()
    });
    select_text_with_mouse(&mut tab);
    assert_eq!(
        copy_events(&plugin_receiver),
        vec![Event::SystemClipboardFailure]
    );
    // A failure must not unexpectedly publish automatic text to clipboard history.
    assert!(!explicit.exists());
    tab.copy_selection(1).unwrap();
    wait_for_copy(&explicit, "selected text");
}

fn new_tab(options: CopyOptions) -> (Tab, Receiver<(PluginInstruction, ErrorContext)>) {
    let (mut tab, receiver) =
        create_new_tab_with_copy_options(Size { cols: 80, rows: 20 }, true, options);
    tab.set_pane_frames(PaneFrameStyle::None);
    tab.handle_pty_bytes(1, b"selected text".to_vec()).unwrap();
    (tab, receiver)
}

fn select_text_with_mouse(tab: &mut Tab) {
    for event in [
        MouseEvent::new_left_press_event(Position::new(0, 0)),
        MouseEvent::new_left_motion_event(Position::new(0, 13)),
        MouseEvent::new_left_release_event(Position::new(0, 13)),
    ] {
        tab.handle_mouse_event(&event, 1).unwrap();
    }
    assert_eq!(
        tab.get_active_pane(1)
            .unwrap()
            .get_selected_text(1)
            .as_deref(),
        Some("selected text")
    );
}

fn copy_command(dir: &Path, destination: &Path) -> String {
    let script = dir.join("copy.sh");
    std::fs::write(&script, "cat > \"$1\"\n").unwrap();
    format!("sh {} {}", script.display(), destination.display())
}

fn wait_for_copy(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(path).ok().as_deref() == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "copy did not reach {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn copy_events(receiver: &Receiver<(PluginInstruction, ErrorContext)>) -> Vec<Event> {
    receiver
        .try_iter()
        .flat_map(|(instruction, _)| match instruction {
            PluginInstruction::Update(updates) => updates
                .into_iter()
                .filter_map(|(_, _, event)| match event {
                    Event::CopyToClipboard(_) | Event::SystemClipboardFailure => Some(event),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        })
        .collect()
}

fn has_osc52(receiver: &Receiver<(ServerInstruction, ErrorContext)>) -> bool {
    receiver
        .try_iter()
        .any(|(instruction, _)| match instruction {
            ServerInstruction::Render(Some(output)) => {
                output.values().any(|text| text.contains("\x1b]52;"))
            },
            _ => false,
        })
}
