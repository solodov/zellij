use std::process::{Child, Command, Stdio};

/// Move the host mouse pointer by a relative pixel delta through Hammerspoon.
/// Returns true when a move request was spawned.
pub fn move_mouse_by(dx: isize, dy: isize) -> bool {
    let Some(script) = hammerspoon_mouse_position_script(dx, dy) else {
        return false;
    };

    let mut command = Command::new("hs");
    command
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match command.spawn() {
        Ok(process) => {
            reap_mouse_positioner(process);
            true
        },
        Err(e) => {
            log::error!("Failed to spawn mouse positioner: {}", e);
            false
        },
    }
}

fn hammerspoon_mouse_position_script(dx: isize, dy: isize) -> Option<String> {
    if dx == 0 && dy == 0 {
        return None;
    }
    Some(format!("mouseMovement.moveRelative({dx}, {dy})"))
}

fn reap_mouse_positioner(mut process: Child) {
    std::thread::spawn(move || {
        if let Err(e) = process.wait() {
            log::error!("mouse positioner failed: {}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::hammerspoon_mouse_position_script;

    #[test]
    fn hammerspoon_mouse_position_script_skips_zero_delta() {
        assert_eq!(hammerspoon_mouse_position_script(0, 0), None);
    }

    #[test]
    fn hammerspoon_mouse_position_script_encodes_relative_delta() {
        assert_eq!(
            hammerspoon_mouse_position_script(-120, 40).as_deref(),
            Some("mouseMovement.moveRelative(-120, 40)")
        );
    }
}
