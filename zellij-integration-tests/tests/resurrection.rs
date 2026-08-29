#![cfg(unix)]

use insta::assert_snapshot;
use zellij_integration_tests::{
    keys, normalized, LayoutInfo, TestRunner, TestSession, PROMPT, TERMINAL_SIZE,
};

const RESURRECT_LAYOUT: &str = r#"
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="tab-bar"
        }
        children
        pane size=1 borderless=true {
            plugin location="status-bar"
        }
    }
    tab name="alpha" {
        pane
    }
    tab name="beta" focus=true {
        pane
    }
}
"#;

fn start_serializing_session(extra_config: &str) -> TestSession {
    TestRunner::new(TERMINAL_SIZE)
        .with_config(&format!("session_serialization true\n{}", extra_config))
        .with_layout(LayoutInfo::Stringified(RESURRECT_LAYOUT.to_string()))
        .start()
}

fn wait_for_layout_loaded(zellij: &TestSession) {
    zellij.wait_until("layout loaded with both tabs", |grid_snapshot| {
        grid_snapshot.contains("alpha")
            && grid_snapshot.contains("beta")
            && grid_snapshot.status_bar_appears()
    });
}

#[test]
fn quit_and_resurrect_session() {
    let mut zellij = start_serializing_session("");
    wait_for_layout_loaded(&zellij);

    zellij.send_stdin(&keys::ctrl('p'));
    zellij.send_stdin(&keys::key('r'));
    let new_pane = zellij.expect_pty_spawn();
    new_pane.output(PROMPT);
    zellij.wait_until("new pane opened before serialization", |grid_snapshot| {
        grid_snapshot.contains("│")
    });

    zellij.save_session();
    zellij.wait_for_serialized_session();
    zellij.quit();

    zellij.resurrect(TERMINAL_SIZE);
    let grid_snapshot = zellij.wait_until(
        "resurrected session restored the runtime pane",
        |grid_snapshot| {
            grid_snapshot.contains("Zellij (test")
                && grid_snapshot.contains("alpha")
                && grid_snapshot.contains("beta")
                && grid_snapshot.contains("│")
                && grid_snapshot.status_bar_appears()
        },
    );
    assert_snapshot!(normalized(&grid_snapshot));
    zellij.quit();
}

#[test]
fn quitting_session_saves_final_resurrection_state_without_waiting_for_interval() {
    let mut zellij = start_serializing_session("serialization_interval 1000");
    wait_for_layout_loaded(&zellij);

    zellij.send_stdin(&keys::ctrl('p'));
    zellij.send_stdin(&keys::key('r'));
    let new_pane = zellij.expect_pty_spawn();
    new_pane.output(PROMPT);
    zellij.wait_until("new pane opened before final save", |grid_snapshot| {
        grid_snapshot.contains("│")
    });

    zellij.quit();
    zellij.resurrect(TERMINAL_SIZE);
    zellij.wait_until(
        "resurrected session restored the pane from the final save",
        |grid_snapshot| grid_snapshot.contains("│") && grid_snapshot.status_bar_appears(),
    );
    zellij.quit();
}

#[test]
fn closing_all_panes_forgets_resurrection_state() {
    let mut zellij = start_serializing_session("");
    wait_for_layout_loaded(&zellij);
    zellij.save_session();
    zellij.wait_for_serialized_session();

    let layout_path = zellij_utils::consts::session_layout_cache_file_name(zellij.session_name());
    assert!(
        layout_path.exists(),
        "expected saved resurrection layout at {}",
        layout_path.display()
    );

    zellij.send_stdin(&keys::ctrl('p'));
    zellij.send_stdin(&keys::key('x'));
    zellij.wait_until("first tab was closed", |grid_snapshot| {
        grid_snapshot.contains("alpha") && !grid_snapshot.contains("beta")
    });

    zellij.send_stdin(&keys::ctrl('p'));
    zellij.send_stdin(&keys::key('x'));
    zellij.wait_for_exit();

    assert!(
        !layout_path.exists(),
        "closing all panes should delete resurrection layout at {}",
        layout_path.display()
    );
}

#[test]
fn quit_and_resurrect_session_with_viewport_serialization() {
    let mut zellij = TestRunner::new(TERMINAL_SIZE)
        .with_config("session_serialization true\nserialize_pane_viewport true")
        .start();
    let terminal = zellij.expect_pty_spawn();
    terminal.output(b"VIEWPORT_CONTENT_TO_RESTORE\n");
    zellij.wait_until(
        "pane content rendered before serialization",
        |grid_snapshot| {
            grid_snapshot.contains("VIEWPORT_CONTENT_TO_RESTORE")
                && grid_snapshot.status_bar_appears()
        },
    );

    zellij.save_session();
    zellij.wait_for_serialized_session();
    zellij.quit();

    zellij.resurrect(TERMINAL_SIZE);
    let grid_snapshot =
        zellij.wait_until("resurrected viewport and chrome settled", |grid_snapshot| {
            grid_snapshot.contains("VIEWPORT_CONTENT_TO_RESTORE")
                && grid_snapshot.contains("Zellij (test")
                && grid_snapshot.tab_bar_appears()
                && grid_snapshot.status_bar_appears()
        });
    assert_snapshot!(normalized(&grid_snapshot));
    zellij.quit();
}
