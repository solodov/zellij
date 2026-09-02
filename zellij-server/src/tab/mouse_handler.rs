use std::collections::HashSet;
use std::time::Instant;
use zellij_utils::data::{Direction, InputMode, Resize, ResizeStrategy};
use zellij_utils::errors::prelude::*;
use zellij_utils::input::mouse::{MouseEvent, MouseEventType};
use zellij_utils::pane_size::{PaneGeom, Size};
use zellij_utils::position::Position;

use crate::background_jobs::BackgroundJob;
use crate::output::{CharacterChunk, Output};
use crate::panes::{AnsiCode, PaneId, RcCharacterStyles, TerminalCharacter, TextPlumbPayload};
use crate::plugins::PluginInstruction;
use crate::pty::PtyInstruction;
use crate::screen::{GuestModalOutcome, ScreenInstruction};
use crate::{ClientId, ServerInstruction};

use super::{Pane, Tab};

fn clear_hover_for_client(tab: &mut Tab, client_id: ClientId) -> bool {
    let mut cleared = false;
    if let Some(prev_pid) = tab.mouse_hover_pane_id.remove(&client_id) {
        if let Some(pane) = tab.get_pane_with_id_mut(prev_pid) {
            pane.set_hover_position(None);
        }
        cleared = true;
    }
    if let Some(prev_plugin_pid) = tab.plugin_hover_pane_id.remove(&client_id) {
        if let Some(pane) = tab.get_pane_with_id(prev_plugin_pid) {
            let _ = pane.mouse_event(
                &MouseEvent::new_buttonless_motion(Position::new(0, u16::MAX)),
                client_id,
            );
        }
        cleared = true;
    }
    cleared
}

#[derive(Debug, Default, Copy, Clone)]
pub struct MouseEffect {
    pub state_changed: bool,
    pub leave_clipboard_message: bool,
    pub kill_session_if_no_selectable_panes: bool,
    pub group_toggle: Option<PaneId>,
    pub group_add: Option<PaneId>,
    pub ungroup: bool,
    pub suppress_scroll_mode_sync: bool,
}

impl MouseEffect {
    pub fn state_changed() -> Self {
        MouseEffect {
            state_changed: true,
            ..Default::default()
        }
    }
    pub fn state_changed_and_kill_session_if_no_selectable_panes() -> Self {
        MouseEffect {
            state_changed: true,
            kill_session_if_no_selectable_panes: true,
            ..Default::default()
        }
    }
    pub fn leave_clipboard_message() -> Self {
        MouseEffect {
            leave_clipboard_message: true,
            ..Default::default()
        }
    }
    pub fn state_changed_and_leave_clipboard_message() -> Self {
        MouseEffect {
            state_changed: true,
            leave_clipboard_message: true,
            ..Default::default()
        }
    }
    pub fn group_toggle(pane_id: PaneId) -> Self {
        MouseEffect {
            state_changed: true,
            group_toggle: Some(pane_id),
            ..Default::default()
        }
    }
    pub fn group_add(pane_id: PaneId) -> Self {
        MouseEffect {
            state_changed: true,
            group_add: Some(pane_id),
            ..Default::default()
        }
    }
    pub fn ungroup() -> Self {
        MouseEffect {
            state_changed: true,
            ungroup: true,
            ..Default::default()
        }
    }
    pub fn suppress_scroll_mode_sync(mut self) -> Self {
        self.suppress_scroll_mode_sync = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
enum MouseAction {
    GroupToggle(PaneId),
    GroupAdd(PaneId),
    Ungroup,
    StartResize {
        pane_id: PaneId,
        edge: PaneEdge,
        is_floating: bool,
        position: Position,
    },
    ContinueResize {
        position: Position,
    },
    StopResize {
        position: Position,
    },
    FocusPane {
        pane_id: PaneId,
        position: Position,
    },
    StartAcmeHandleDrag {
        pane_id: PaneId,
        position: Position,
    },
    ContinueAcmeHandleDrag {
        position: Position,
    },
    StopAcmeHandleDrag {
        position: Position,
    },
    StartAcmeContextMenu {
        pane_id: PaneId,
        position: Position,
    },
    UpdateAcmeContextMenu {
        position: Position,
    },
    FinishAcmeContextMenu {
        position: Position,
    },
    NewAcmePane {
        pane_id: PaneId,
    },
    NewAcmeColumn {
        pane_id: PaneId,
    },
    EqualizeAcmePaneRows {
        pane_id: PaneId,
    },
    EqualizeAcmeColumns {
        pane_id: PaneId,
        edge: PaneEdge,
    },
    SwapAcmeColumn {
        pane_id: PaneId,
        direction: Direction,
    },
    AcmeClosePane {
        pane_id: PaneId,
    },
    TogglePaneWrap {
        pane_id: PaneId,
    },
    PasteFromHostClipboard {
        pane_id: PaneId,
    },
    PlumbText {
        pane_id: PaneId,
        position: Position,
    },
    FocusPaneAndClickThrough {
        pane_id: PaneId,
        position: Position,
        event: MouseEvent,
    },
    ShowFloatingPanesAndFocus {
        pane_id: PaneId,
    },
    StartSelection {
        pane_id: PaneId,
        position: Position,
    },
    UpdateSelection {
        position: Position,
    },
    EndSelection {
        position: Position,
    },
    StartMovingFloatingPane {
        position: Position,
    },
    ContinueMovingFloatingPane {
        position: Position,
    },
    StopMovingFloatingPane {
        position: Position,
    },
    ScrollUp {
        pane_id: PaneId,
        lines: usize,
    },
    ScrollDown {
        pane_id: PaneId,
        lines: usize,
    },
    ScrollLeft {
        pane_id: PaneId,
        cols: usize,
    },
    ScrollRight {
        pane_id: PaneId,
        cols: usize,
    },
    ResizeScrollUp {
        pane_id: PaneId,
    },
    ResizeScrollDown {
        pane_id: PaneId,
    },
    ScrollToPreviousPrompt {
        pane_id: PaneId,
    },
    ScrollToNextPrompt {
        pane_id: PaneId,
    },
    UpdateHover {
        pane_id: Option<PaneId>,
        position: Option<Position>,
    },
    FocusOnHover {
        pane_id: PaneId,
        position: Position,
    },
    SearchDown,
    SearchUp,
    CancelSearch,
    SendToTerminal {
        pane_id: PaneId,
        event: MouseEvent,
    },
    FrameIntercepted {
        pane_id: PaneId,
    },
    NoAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneResizeState {
    pub pane_id: PaneId,
    pub edge: PaneEdge,
    pub start_position: Position,
    pub start_geom: PaneGeom,
    pub is_floating: bool,
    pub acme_resize_snapshot: Option<Vec<(PaneId, PaneGeom)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AcmeHandleDragState {
    pane_id: PaneId,
    start_position: Position,
    is_dragging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AcmeContextMenuState {
    pane_id: PaneId,
    anchor_position: Position,
    x: usize,
    y: usize,
    selected_action: Option<AcmeContextMenuAction>,
    drag_offset: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcmeContextMenuAction {
    Put,
    Send,
    Look,
    GoToDefinition,
    Cancel,
}

impl AcmeContextMenuAction {
    fn is_drag_targeted(self) -> bool {
        matches!(self, Self::Put | Self::Send | Self::Look)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AcmeHoverHelpState {
    target: AcmeHoverHelpTarget,
    position: Position,
    first_seen: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcmeHoverHelpTarget {
    PaneFrame { edge: Option<PaneEdge> },
    AcmeTitle,
    AcmeTitleButton,
    AcmeTabSquare,
    AcmeTab,
}

const ACME_HANDLE_DRAG_THRESHOLD: usize = 1;
const ACME_HOVER_HELP_DELAY_MS: u128 = 1400;
const ACME_CONTEXT_MENU_LABEL_WIDTH: usize = 10;
const ACME_CONTEXT_MENU_CONTENT_WIDTH: usize = ACME_CONTEXT_MENU_LABEL_WIDTH + 2;
const ACME_CONTEXT_MENU_WIDTH: usize = ACME_CONTEXT_MENU_CONTENT_WIDTH + 2;
const ACME_CONTEXT_MENU_ITEM_COUNT: usize = 5;
const ACME_CONTEXT_MENU_HEIGHT: usize = ACME_CONTEXT_MENU_ITEM_COUNT + 2;
const ACME_CONTEXT_MENU_ITEMS: [(AcmeContextMenuAction, &str); ACME_CONTEXT_MENU_ITEM_COUNT] = [
    (AcmeContextMenuAction::Put, "put"),
    (AcmeContextMenuAction::Send, "send"),
    (AcmeContextMenuAction::Look, "look"),
    (AcmeContextMenuAction::GoToDefinition, "definition"),
    (AcmeContextMenuAction::Cancel, "cancel"),
];

impl AcmeContextMenuState {
    fn new(pane_id: PaneId, anchor_position: Position, display_size: Size) -> Self {
        let anchor_x = anchor_position.column();
        let anchor_y = anchor_position.line().max(0) as usize;
        let x = acme_context_menu_x(anchor_x, display_size.cols);
        let y = anchor_y.min(display_size.rows.saturating_sub(ACME_CONTEXT_MENU_HEIGHT));
        AcmeContextMenuState {
            pane_id,
            anchor_position,
            x,
            y,
            selected_action: None,
            drag_offset: None,
        }
    }

    fn update_for_mouse_position(&mut self, position: Position, display_size: Size) -> bool {
        let previous = *self;
        if self.contains_position(position) {
            if let Some(action) = self.action_at(position) {
                self.selected_action = Some(action);
                if action.is_drag_targeted() {
                    self.drag_offset = Some((
                        position.column().saturating_sub(self.x),
                        usize::try_from(position.line())
                            .unwrap_or_default()
                            .saturating_sub(self.y),
                    ));
                } else {
                    self.drag_offset = None;
                }
            }
        } else if let Some((offset_x, offset_y)) = self.drag_offset {
            self.x = position
                .column()
                .saturating_sub(offset_x)
                .min(display_size.cols.saturating_sub(ACME_CONTEXT_MENU_WIDTH));
            self.y = usize::try_from(position.line())
                .unwrap_or_default()
                .saturating_sub(offset_y)
                .min(display_size.rows.saturating_sub(ACME_CONTEXT_MENU_HEIGHT));
            self.selected_action = self
                .action_at(position)
                .filter(|action| action.is_drag_targeted())
                .or(self
                    .selected_action
                    .filter(|action| action.is_drag_targeted()));
        } else {
            self.selected_action = None;
        }
        *self != previous
    }

    fn action_for_release(&self, position: Position) -> Option<AcmeContextMenuAction> {
        self.selected_action
            .filter(|action| action.is_drag_targeted())
            .or_else(|| self.action_at(position))
    }

    fn contains_position(&self, position: Position) -> bool {
        let Ok(line) = usize::try_from(position.line()) else {
            return false;
        };
        let column = position.column();
        column >= self.x
            && column < self.x.saturating_add(ACME_CONTEXT_MENU_WIDTH)
            && line >= self.y
            && line < self.y.saturating_add(ACME_CONTEXT_MENU_HEIGHT)
    }

    fn action_at(&self, position: Position) -> Option<AcmeContextMenuAction> {
        let line = usize::try_from(position.line()).ok()?;
        let column = position.column();
        if column <= self.x || column >= self.x.saturating_add(ACME_CONTEXT_MENU_WIDTH - 1) {
            return None;
        }
        if line <= self.y || line >= self.y.saturating_add(ACME_CONTEXT_MENU_HEIGHT - 1) {
            return None;
        }
        ACME_CONTEXT_MENU_ITEMS
            .get(line - self.y - 1)
            .map(|(action, _label)| *action)
    }
}

fn acme_context_menu_x(anchor_x: usize, screen_cols: usize) -> usize {
    if screen_cols <= ACME_CONTEXT_MENU_WIDTH {
        return 0;
    }
    let right_of_anchor = anchor_x.saturating_add(1);
    if right_of_anchor.saturating_add(ACME_CONTEXT_MENU_WIDTH) <= screen_cols {
        right_of_anchor
    } else {
        anchor_x.saturating_sub(ACME_CONTEXT_MENU_WIDTH)
    }
}

fn non_blank_text(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn text_with_trailing_enter(mut text: String) -> Vec<u8> {
    if !text.ends_with('\n') && !text.ends_with('\r') {
        text.push('\r');
    }
    text.into_bytes()
}

fn acme_context_menu_character_chunks(menu: AcmeContextMenuState) -> Vec<CharacterChunk> {
    let mut chunks = vec![CharacterChunk::new(
        acme_context_menu_border_row(true),
        menu.x,
        menu.y,
    )];
    for (index, (action, label)) in ACME_CONTEXT_MENU_ITEMS.iter().enumerate() {
        let selected = menu.selected_action == Some(*action);
        chunks.push(CharacterChunk::new(
            acme_context_menu_item_row(label, selected),
            menu.x,
            menu.y + index + 1,
        ));
    }
    chunks.push(CharacterChunk::new(
        acme_context_menu_border_row(false),
        menu.x,
        menu.y + ACME_CONTEXT_MENU_HEIGHT - 1,
    ));
    chunks
}

fn acme_context_menu_border_row(top: bool) -> Vec<TerminalCharacter> {
    let style = acme_context_menu_border_style();
    let (left, right) = if top { ('┌', '┐') } else { ('└', '┘') };
    std::iter::once(left)
        .chain(std::iter::repeat('─').take(ACME_CONTEXT_MENU_CONTENT_WIDTH))
        .chain(std::iter::once(right))
        .map(|character| TerminalCharacter::new_singlewidth_styled(character, style.clone()))
        .collect()
}

fn acme_context_menu_item_row(label: &str, selected: bool) -> Vec<TerminalCharacter> {
    let border_style = acme_context_menu_border_style();
    let item_style = acme_context_menu_item_style(selected);
    let content = format!(" {:<width$} ", label, width = ACME_CONTEXT_MENU_LABEL_WIDTH);
    let mut row = vec![TerminalCharacter::new_singlewidth_styled(
        '│',
        border_style.clone(),
    )];
    row.extend(
        content.chars().map(|character| {
            TerminalCharacter::new_singlewidth_styled(character, item_style.clone())
        }),
    );
    row.push(TerminalCharacter::new_singlewidth_styled('│', border_style));
    row
}

fn acme_context_menu_border_style() -> RcCharacterStyles {
    let mut style = RcCharacterStyles::reset();
    style.update(|style| {
        style.background = Some(AnsiCode::RgbCode((0xe4, 0xf6, 0xd3)));
        style.foreground = Some(AnsiCode::RgbCode((0x1f, 0x5b, 0x2a)));
        style.bold = Some(AnsiCode::Reset);
        style.italic = Some(AnsiCode::Reset);
    });
    style
}

fn acme_context_menu_item_style(selected: bool) -> RcCharacterStyles {
    let mut style = RcCharacterStyles::reset();
    style.update(|style| {
        if selected {
            style.background = Some(AnsiCode::RgbCode((0x1f, 0x5b, 0x2a)));
            style.foreground = Some(AnsiCode::RgbCode((0xe4, 0xf6, 0xd3)));
            style.bold = Some(AnsiCode::On);
        } else {
            style.background = Some(AnsiCode::RgbCode((0xe4, 0xf6, 0xd3)));
            style.foreground = Some(AnsiCode::RgbCode((0x1f, 0x5b, 0x2a)));
            style.bold = Some(AnsiCode::Reset);
        }
        style.italic = Some(AnsiCode::Reset);
    });
    style
}

fn acme_hover_help_is_visible(help: AcmeHoverHelpState) -> bool {
    help.first_seen.elapsed().as_millis() >= ACME_HOVER_HELP_DELAY_MS
}

fn acme_hover_help_character_chunks(
    help: AcmeHoverHelpState,
    display_size: Size,
) -> Vec<CharacterChunk> {
    let lines = acme_hover_help_lines(help.target);
    let content_width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let width = content_width + 2;
    let height = lines.len() + 2;
    let (x, y) = acme_hover_help_position(help.position, display_size, width, height);

    let mut chunks = vec![CharacterChunk::new(
        acme_hover_help_border_row(true, content_width),
        x,
        y,
    )];
    for (index, line) in lines.iter().enumerate() {
        chunks.push(CharacterChunk::new(
            acme_hover_help_text_row(line, content_width),
            x,
            y + index + 1,
        ));
    }
    chunks.push(CharacterChunk::new(
        acme_hover_help_border_row(false, content_width),
        x,
        y + height - 1,
    ));
    chunks
}

fn acme_hover_help_position(
    position: Position,
    display_size: Size,
    width: usize,
    height: usize,
) -> (usize, usize) {
    let right_of_cursor = position.column().saturating_add(1);
    let x = if right_of_cursor.saturating_add(width) <= display_size.cols {
        right_of_cursor
    } else {
        position.column().saturating_sub(width)
    };
    let below_cursor = usize::try_from(position.line())
        .unwrap_or_default()
        .saturating_add(1);
    let y = if below_cursor.saturating_add(height) <= display_size.rows {
        below_cursor
    } else {
        usize::try_from(position.line())
            .unwrap_or_default()
            .saturating_sub(height)
    };
    (
        x.min(display_size.cols.saturating_sub(width)),
        y.min(display_size.rows.saturating_sub(height)),
    )
}

fn position_is_on_display_edge(position: Position, display_size: Size) -> bool {
    let Ok(line) = usize::try_from(position.line()) else {
        return true;
    };
    display_size.cols == 0
        || display_size.rows == 0
        || position.column() == 0
        || position.column() >= display_size.cols.saturating_sub(1)
        || line == 0
        || line >= display_size.rows.saturating_sub(1)
}

fn acme_hover_help_border_row(top: bool, content_width: usize) -> Vec<TerminalCharacter> {
    let style = acme_hover_help_border_style();
    let (left, right) = if top { ('┌', '┐') } else { ('└', '┘') };
    std::iter::once(left)
        .chain(std::iter::repeat('─').take(content_width))
        .chain(std::iter::once(right))
        .map(|character| TerminalCharacter::new_singlewidth_styled(character, style.clone()))
        .collect()
}

fn acme_hover_help_text_row(line: &str, content_width: usize) -> Vec<TerminalCharacter> {
    let border_style = acme_hover_help_border_style();
    let text_style = acme_hover_help_text_style();
    let content = format!(
        " {:<width$} ",
        line,
        width = content_width.saturating_sub(2)
    );
    let mut row = vec![TerminalCharacter::new_singlewidth_styled(
        '│',
        border_style.clone(),
    )];
    row.extend(
        content.chars().map(|character| {
            TerminalCharacter::new_singlewidth_styled(character, text_style.clone())
        }),
    );
    row.push(TerminalCharacter::new_singlewidth_styled('│', border_style));
    row
}

fn acme_hover_help_border_style() -> RcCharacterStyles {
    acme_hover_help_style()
}

fn acme_hover_help_text_style() -> RcCharacterStyles {
    acme_hover_help_style()
}

fn acme_hover_help_style() -> RcCharacterStyles {
    let mut style = RcCharacterStyles::reset();
    style.update(|style| {
        style.background = Some(AnsiCode::RgbCode((0xff, 0xf3, 0xdd)));
        style.foreground = Some(AnsiCode::RgbCode((0x8a, 0x4f, 0x1d)));
        style.bold = Some(AnsiCode::Reset);
        style.italic = Some(AnsiCode::Reset);
    });
    style
}

fn acme_hover_help_target(ctx: &MouseEventContext) -> Option<AcmeHoverHelpTarget> {
    if ctx.acme_title_button_pane_id.is_some() {
        return Some(AcmeHoverHelpTarget::AcmeTitleButton);
    }
    if ctx.acme_title_pane_id.is_some() {
        return Some(AcmeHoverHelpTarget::AcmeTitle);
    }
    if let Some(details) = ctx.clicked_pane {
        if details.on_frame {
            return Some(AcmeHoverHelpTarget::PaneFrame { edge: details.edge });
        }
    }
    None
}

fn acme_hover_help_lines(target: AcmeHoverHelpTarget) -> Vec<&'static str> {
    match target {
        AcmeHoverHelpTarget::PaneFrame { edge } => {
            if edge.is_some() {
                vec!["left drag: resize", "ctrl-scroll: resize", "right: no-op"]
            } else {
                vec!["left drag: move", "right: no-op"]
            }
        },
        AcmeHoverHelpTarget::AcmeTitle => vec![
            "ctrl-left: equalize rows",
            "alt-left/right: swap column",
            "right: no-op",
        ],
        AcmeHoverHelpTarget::AcmeTitleButton => vec![
            "left drag: move",
            "ctrl-left: new pane",
            "ctrl-right: new column",
            "middle: close",
        ],
        AcmeHoverHelpTarget::AcmeTabSquare => {
            vec!["left: switch/drag", "ctrl-right: new tab", "middle: close"]
        },
        AcmeHoverHelpTarget::AcmeTab => vec!["left: switch", "drag square: reorder"],
    }
}

impl AcmeHoverHelpState {
    fn new(target: AcmeHoverHelpTarget, position: Position) -> Self {
        AcmeHoverHelpState {
            target,
            position,
            first_seen: Instant::now(),
        }
    }

    #[cfg(test)]
    pub(super) fn visible_for_tests(target: AcmeHoverHelpTarget, position: Position) -> Self {
        AcmeHoverHelpState {
            target,
            position,
            first_seen: Instant::now()
                - std::time::Duration::from_millis((ACME_HOVER_HELP_DELAY_MS + 1) as u64),
        }
    }
}

fn schedule_acme_hover_help(
    tab: &mut Tab,
    target: AcmeHoverHelpTarget,
    position: Position,
    client_id: ClientId,
) -> Result<bool> {
    let was_visible = tab
        .acme_hover_help
        .get(&client_id)
        .copied()
        .map(acme_hover_help_is_visible)
        .unwrap_or(false);
    if was_visible {
        tab.set_force_render();
    }
    tab.acme_hover_help
        .insert(client_id, AcmeHoverHelpState::new(target, position));
    tab.senders
        .send_to_background_jobs(BackgroundJob::ShowAcmeHoverHelp { client_id })
        .context("failed to schedule Acme hover help")?;
    Ok(was_visible)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClickedPaneDetails {
    pane_id: PaneId,
    on_frame: bool,
    frame_intercepted: bool,
    edge: Option<PaneEdge>,
    is_acme_title: bool,
    is_floating: bool,
    terminal_wants_mouse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MouseEventContext {
    pane_id_at_position: Option<PaneId>,
    active_pane_id: Option<PaneId>,
    floating_visible: bool,
    input_mode: InputMode,
    pane_being_resized: bool,
    selecting_with_mouse: bool,
    pane_being_moved: bool,
    acme_handle_drag: Option<AcmeHandleDragState>,
    acme_context_menu: Option<AcmeContextMenuState>,
    clicked_pane: Option<ClickedPaneDetails>,
    advanced_mouse_actions: bool,
    acme_vertical_border_hit: bool,
    acme_title_pane_id: Option<PaneId>,
    acme_title_button_pane_id: Option<PaneId>,
    acme_wrap_indicator_pane_id: Option<PaneId>,
    pinned_selectable: Option<PaneId>,
    pinned_unselectable: Option<PaneId>,
    focus_follows_mouse: bool,
    mouse_click_through: bool,
    mouse_scroll_resize: bool,
    passthrough_pane_id: Option<PaneId>,
}

fn position_is_on_vertical_frame(pane: &dyn Pane, position: &Position) -> bool {
    if !pane.contains(position) {
        return false;
    }
    let left_frame = pane.x()..pane.get_content_x();
    let right_frame = pane.get_content_x() + pane.get_content_columns()..pane.x() + pane.cols();
    left_frame.contains(&position.column()) || right_frame.contains(&position.column())
}

fn acme_handle_drag_delta(start: Position, current: Position) -> (usize, usize) {
    let line_delta = if start.line() >= current.line() {
        (start.line() - current.line()) as usize
    } else {
        (current.line() - start.line()) as usize
    };
    let column_delta = start.column().abs_diff(current.column());
    (line_delta, column_delta)
}

fn acme_handle_drag_exceeded_threshold(start: Position, current: Position) -> bool {
    let (line_delta, column_delta) = acme_handle_drag_delta(start, current);
    line_delta > ACME_HANDLE_DRAG_THRESHOLD || column_delta > ACME_HANDLE_DRAG_THRESHOLD
}

fn edge_and_delta_to_strategies(
    edge: PaneEdge,
    delta_x: isize,
    delta_y: isize,
) -> Vec<ResizeStrategy> {
    use Direction::*;
    use Resize::*;

    match edge {
        PaneEdge::Left => {
            let resize = if delta_x < 0 { Increase } else { Decrease };
            vec![ResizeStrategy {
                resize,
                direction: Some(Left),
                invert_on_boundaries: false,
            }]
        },
        PaneEdge::Right => {
            let resize = if delta_x > 0 { Increase } else { Decrease };
            vec![ResizeStrategy {
                resize,
                direction: Some(Right),
                invert_on_boundaries: false,
            }]
        },
        PaneEdge::Top => {
            let resize = if delta_y < 0 { Increase } else { Decrease };
            vec![ResizeStrategy {
                resize,
                direction: Some(Up),
                invert_on_boundaries: false,
            }]
        },
        PaneEdge::Bottom => {
            let resize = if delta_y > 0 { Increase } else { Decrease };
            vec![ResizeStrategy {
                resize,
                direction: Some(Down),
                invert_on_boundaries: false,
            }]
        },
        PaneEdge::TopLeft => {
            let mut strategies = vec![];
            let resize_y = if delta_y < 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_y,
                direction: Some(Up),
                invert_on_boundaries: false,
            });
            let resize_x = if delta_x < 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_x,
                direction: Some(Left),
                invert_on_boundaries: false,
            });
            strategies
        },
        PaneEdge::TopRight => {
            let mut strategies = vec![];
            let resize_y = if delta_y < 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_y,
                direction: Some(Up),
                invert_on_boundaries: false,
            });
            let resize_x = if delta_x > 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_x,
                direction: Some(Right),
                invert_on_boundaries: false,
            });
            strategies
        },
        PaneEdge::BottomLeft => {
            let mut strategies = vec![];
            let resize_y = if delta_y > 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_y,
                direction: Some(Down),
                invert_on_boundaries: false,
            });
            let resize_x = if delta_x < 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_x,
                direction: Some(Left),
                invert_on_boundaries: false,
            });
            strategies
        },
        PaneEdge::BottomRight => {
            let mut strategies = vec![];
            let resize_y = if delta_y > 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_y,
                direction: Some(Down),
                invert_on_boundaries: false,
            });
            let resize_x = if delta_x > 0 { Increase } else { Decrease };
            strategies.push(ResizeStrategy {
                resize: resize_x,
                direction: Some(Right),
                invert_on_boundaries: false,
            });
            strategies
        },
    }
}

pub struct MouseHandler;

impl MouseHandler {
    pub(crate) fn handle_mouse_event(
        tab: &mut Tab,
        event: &MouseEvent,
        client_id: ClientId,
        passthrough_pane_id: Option<PaneId>,
    ) -> Result<MouseEffect> {
        let context_menu_is_active = tab.acme_context_menus.contains_key(&client_id);
        if !event.right && !context_menu_is_active {
            if let Some(effect) = Self::intercept_guest_modal_mouse_event(tab, event, client_id)? {
                return Ok(effect);
            }
        }
        let context = Self::gather_mouse_event_context(tab, event, client_id, passthrough_pane_id)?;
        let action = Self::determine_mouse_action(event, &context)?;
        let mut effect = Self::execute_mouse_action(tab, action, event, client_id)?;
        if Self::update_acme_hover_help(tab, event, &context, client_id)? {
            effect.state_changed = true;
        }
        Ok(effect)
    }

    pub(super) fn acme_hover_help_visible_for_client(tab: &Tab, client_id: ClientId) -> bool {
        tab.acme_hover_help
            .get(&client_id)
            .copied()
            .map(acme_hover_help_is_visible)
            .unwrap_or(false)
    }

    pub(super) fn render_acme_hover_help(
        tab: &Tab,
        output: &mut Output,
        client_id_override: Option<ClientId>,
    ) -> Result<()> {
        let mut clients: HashSet<ClientId> =
            tab.connected_clients.borrow().iter().copied().collect();
        if let Some(client_id) = client_id_override {
            clients.insert(client_id);
        }
        for client_id in clients {
            if let Some(help) = tab.acme_hover_help.get(&client_id).copied() {
                if acme_hover_help_is_visible(help) {
                    output.add_character_chunks_to_client(
                        client_id,
                        acme_hover_help_character_chunks(help, tab.size),
                        Some(usize::MAX),
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn update_acme_tab_bar_hover_help(
        tab: &mut Tab,
        client_id: ClientId,
        position: Position,
        target: AcmeHoverHelpTarget,
    ) -> Result<bool> {
        schedule_acme_hover_help(tab, target, position, client_id)
    }

    pub(super) fn render_acme_context_menus(
        tab: &Tab,
        output: &mut Output,
        client_id_override: Option<ClientId>,
    ) -> Result<()> {
        let mut clients: HashSet<ClientId> =
            tab.connected_clients.borrow().iter().copied().collect();
        if let Some(client_id) = client_id_override {
            clients.insert(client_id);
        }
        for client_id in clients {
            if let Some(menu) = tab.acme_context_menus.get(&client_id) {
                output.add_character_chunks_to_client(
                    client_id,
                    acme_context_menu_character_chunks(*menu),
                    Some(usize::MAX),
                )?;
            }
        }
        Ok(())
    }

    fn update_acme_hover_help(
        tab: &mut Tab,
        event: &MouseEvent,
        ctx: &MouseEventContext,
        client_id: ClientId,
    ) -> Result<bool> {
        let mut should_render = tab.mouse_help_text_visible.remove(&client_id).is_some();

        let is_buttonless_motion = event.event_type == MouseEventType::Motion
            && !event.left
            && !event.right
            && !event.middle
            && !event.wheel_up
            && !event.wheel_down
            && !event.wheel_left
            && !event.wheel_right;
        if is_buttonless_motion
            && (position_is_on_display_edge(event.position, tab.size)
                || (tab.pane_frame_style.draws_titles() && event.position.line() == 0))
        {
            if tab.acme_hover_help.remove(&client_id).is_some() {
                tab.set_force_render();
                should_render = true;
            }
            return Ok(should_render);
        }

        if is_buttonless_motion {
            if let Some(target) = acme_hover_help_target(ctx) {
                if schedule_acme_hover_help(tab, target, event.position, client_id)? {
                    should_render = true;
                }
            } else if tab.acme_hover_help.remove(&client_id).is_some() {
                tab.set_force_render();
                should_render = true;
            }
        } else if tab.acme_hover_help.remove(&client_id).is_some() {
            tab.set_force_render();
            should_render = true;
        }

        Ok(should_render)
    }

    fn intercept_guest_modal_mouse_event(
        tab: &mut Tab,
        event: &MouseEvent,
        client_id: ClientId,
    ) -> Result<Option<MouseEffect>> {
        let pane_id_at_position = Self::get_pane_at(tab, &event.position, false)?.map(|p| p.pid());
        let pane_id = match pane_id_at_position {
            Some(pane_id) => pane_id,
            None => return Ok(None),
        };
        if !tab.pane_has_guest_modal_for_client(pane_id, client_id) {
            return Ok(None);
        }
        let hit_option = if event.event_type == MouseEventType::Release && event.left {
            let style = tab.style;
            if let Some(pane) = tab.get_pane_with_id(pane_id) {
                let relative_position = pane.relative_position(&event.position);
                let rows = pane.get_content_rows();
                let columns = pane.get_content_columns();
                let row = relative_position.line();
                if row < 0 {
                    None
                } else {
                    let session_name = pane.guest_session_name().unwrap_or_default();
                    let selection = pane.guest_modal_selection(client_id).unwrap_or(0);
                    let shortcuts = pane.guest_modal_shortcuts();
                    crate::panes::nested_session_modal::guest_modal_option_at_content_row(
                        rows,
                        columns,
                        row as usize,
                        &style,
                        &session_name,
                        selection,
                        &shortcuts,
                    )
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(option) = hit_option {
            let outcome = match option {
                0 => GuestModalOutcome::Zoom,
                _ => GuestModalOutcome::Descend,
            };
            let _ = tab
                .senders
                .send_to_screen(ScreenInstruction::GuestModalChoice {
                    client_id,
                    pane_id,
                    outcome,
                });
        }
        Ok(Some(MouseEffect::state_changed()))
    }

    fn gather_mouse_event_context(
        tab: &mut Tab,
        event: &MouseEvent,
        client_id: ClientId,
        passthrough_pane_id: Option<PaneId>,
    ) -> Result<MouseEventContext> {
        let err_context = || format!("failed to gather context for event {event:?}");

        let pane_id_at_position = Self::get_pane_at(tab, &event.position, false)
            .with_context(err_context)?
            .map(|p| p.pid());
        let active_pane_id = tab.get_active_pane_id(client_id);
        let floating_visible = tab.floating_panes.panes_are_visible();

        let clicked_pane = pane_id_at_position.and_then(|id| {
            Self::gather_clicked_pane_details(
                tab,
                id,
                &event.position,
                active_pane_id,
                event,
                client_id,
            )
        });
        let acme_vertical_border_hit = clicked_pane
            .as_ref()
            .filter(|details| details.on_frame)
            .map(|details| {
                tab.tiled_panes.pane_is_in_acme_column(details.pane_id)
                    && tab
                        .get_pane_with_id(details.pane_id)
                        .map(|pane| position_is_on_vertical_frame(pane, &event.position))
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        let (acme_title_pane_id, acme_title_button_pane_id, acme_wrap_indicator_pane_id) =
            if floating_visible {
                (None, None, None)
            } else {
                (
                    tab.tiled_panes
                        .acme_title_pane_id_at_position(&event.position),
                    tab.tiled_panes
                        .acme_title_button_pane_id_at_position(&event.position),
                    tab.tiled_panes
                        .acme_wrap_indicator_pane_id_at_position(&event.position),
                )
            };

        let (pinned_selectable, pinned_unselectable) = if !floating_visible {
            let selectable = tab
                .floating_panes
                .get_pinned_pane_id_at(&event.position, true)
                .ok()
                .flatten();
            let unselectable = tab
                .floating_panes
                .get_pinned_pane_id_at(&event.position, false)
                .ok()
                .flatten();
            (selectable, unselectable)
        } else {
            (None, None)
        };

        let input_mode = tab
            .mode_info
            .borrow()
            .get(&client_id)
            .map(|mode_info| mode_info.mode)
            .unwrap_or(tab.default_mode_info.mode);

        Ok(MouseEventContext {
            pane_id_at_position,
            active_pane_id,
            floating_visible,
            input_mode,
            pane_being_resized: tab.pane_being_resized_with_mouse.is_some(),
            selecting_with_mouse: tab.selecting_with_mouse_in_pane.is_some(),
            pane_being_moved: tab.floating_panes.pane_is_being_moved_with_mouse(),
            acme_handle_drag: tab.acme_handle_drag,
            acme_context_menu: tab.acme_context_menus.get(&client_id).copied(),
            clicked_pane,
            advanced_mouse_actions: tab.advanced_mouse_actions,
            acme_vertical_border_hit,
            acme_title_pane_id,
            acme_title_button_pane_id,
            acme_wrap_indicator_pane_id,
            pinned_selectable,
            pinned_unselectable,
            focus_follows_mouse: tab.focus_follows_mouse,
            mouse_click_through: tab.mouse_click_through,
            mouse_scroll_resize: tab.mouse_scroll_resize,
            passthrough_pane_id,
        })
    }

    fn gather_clicked_pane_details(
        tab: &mut Tab,
        pane_id: PaneId,
        position: &Position,
        active_pane_id: Option<PaneId>,
        event: &MouseEvent,
        client_id: ClientId,
    ) -> Option<ClickedPaneDetails> {
        let is_floating = tab.floating_panes.panes_contain(&pane_id);
        let is_hidden_stack_list_member = tab.pane_is_hidden_stack_list_member(&pane_id);
        let (on_frame, frame_intercepted, default_edge, terminal_wants_mouse) = {
            let pane = Self::get_pane_at(tab, position, false).ok()??;
            let on_frame = !is_hidden_stack_list_member && pane.position_is_on_frame(position);
            let frame_intercepted =
                on_frame && pane.intercept_mouse_event_on_frame(event, client_id);
            let default_edge = if on_frame {
                pane.get_edge_at_position(position)
            } else {
                None
            };
            let terminal_wants_mouse = if Some(pane_id) == active_pane_id {
                let relative_position = pane.relative_position(position);
                pane.mouse_left_click(&relative_position, false).is_some()
            } else {
                false
            };
            (
                on_frame,
                frame_intercepted,
                default_edge,
                terminal_wants_mouse,
            )
        };

        let acme_title_hit = !is_floating
            && tab.tiled_panes.acme_title_pane_id_at_position(position) == Some(pane_id);
        let acme_title_edge = if acme_title_hit {
            tab.tiled_panes
                .acme_title_edge_at_position(pane_id, position)
        } else {
            None
        };
        let edge = if acme_title_hit {
            acme_title_edge
        } else {
            default_edge
        };

        Some(ClickedPaneDetails {
            pane_id,
            on_frame: on_frame || acme_title_hit,
            frame_intercepted: frame_intercepted && !acme_title_hit,
            edge,
            is_acme_title: acme_title_hit,
            is_floating,
            terminal_wants_mouse: terminal_wants_mouse && !acme_title_hit,
        })
    }

    fn start_acme_handle_drag(
        tab: &mut Tab,
        pane_id: PaneId,
        position: Position,
        client_id: ClientId,
    ) -> Result<()> {
        tab.focus_pane_with_id(pane_id, false, false, client_id)?;
        tab.acme_handle_drag = Some(AcmeHandleDragState {
            pane_id,
            start_position: position,
            is_dragging: false,
        });
        Ok(())
    }

    fn continue_acme_handle_drag(tab: &mut Tab, position: Position) {
        if let Some(drag_state) = tab.acme_handle_drag.as_mut() {
            drag_state.is_dragging = drag_state.is_dragging
                || acme_handle_drag_exceeded_threshold(drag_state.start_position, position);
        }
    }

    fn stop_acme_handle_drag(
        tab: &mut Tab,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let Some(drag_state) = tab.acme_handle_drag.take() else {
            return Ok(MouseEffect::default());
        };
        let is_drag = drag_state.is_dragging
            || acme_handle_drag_exceeded_threshold(drag_state.start_position, position);
        if is_drag {
            if tab.move_or_reorder_acme_pane_with_position(
                drag_state.pane_id,
                drag_state.start_position,
                position,
                client_id,
            ) {
                Ok(MouseEffect::state_changed())
            } else {
                Ok(MouseEffect::default())
            }
        } else {
            tab.focus_pane_with_id(drag_state.pane_id, false, false, client_id)?;
            if tab.acme_toggle_title_button_pane(client_id) {
                tab.move_mouse_from_position_to_pane_control_square(drag_state.pane_id, position);
            }
            Ok(MouseEffect::state_changed())
        }
    }

    fn execute_acme_context_menu_action(
        tab: &mut Tab,
        menu: AcmeContextMenuState,
        action: AcmeContextMenuAction,
        release_position: Position,
        client_id: ClientId,
    ) -> Result<()> {
        match action {
            AcmeContextMenuAction::Put => {
                if let Some(target_pane_id) =
                    Self::terminal_pane_id_at_position(tab, release_position)?
                {
                    if let Some(text) = Self::selected_or_word_for_menu(tab, menu, client_id) {
                        tab.paste_to_pane_id(text.into_bytes(), target_pane_id, None)
                            .context("failed to put context-menu text into pane")?;
                    }
                }
            },
            AcmeContextMenuAction::Send => {
                if let Some(target_pane_id) =
                    Self::terminal_pane_id_at_position(tab, release_position)?
                {
                    if let Some(text) = Self::selected_or_word_for_menu(tab, menu, client_id) {
                        tab.write_to_pane_id(
                            &None,
                            text_with_trailing_enter(text),
                            false,
                            target_pane_id,
                            Some(client_id),
                            None,
                        )
                        .context("failed to send context-menu text to pane")?;
                    }
                }
            },
            AcmeContextMenuAction::Look => {
                if let Some(target_pane_id) =
                    Self::terminal_pane_id_at_position(tab, release_position)?
                {
                    if let Some(text) = Self::selected_or_word_for_menu(tab, menu, client_id) {
                        tab.focus_pane_with_id(target_pane_id, false, false, client_id)?;
                        let search_anchor_position = if target_pane_id == menu.pane_id {
                            menu.anchor_position
                        } else {
                            release_position
                        };
                        if let Some(pane) = tab.get_pane_with_id_mut(target_pane_id) {
                            let relative_position = pane.relative_position(&search_anchor_position);
                            pane.set_search_term_from_position(
                                &text,
                                &relative_position,
                                client_id,
                            );
                        }
                        Self::switch_client_to_search_mode(tab, client_id)?;
                    }
                }
            },
            AcmeContextMenuAction::GoToDefinition => {
                if let Some(text) = Self::selected_or_word_for_menu(tab, menu, client_id) {
                    tab.senders
                        .send_to_pty(PtyInstruction::PlumbText {
                            pane_id: menu.pane_id,
                            text: TextPlumbPayload {
                                text,
                                click_byte_offset: None,
                                action: Some("go-to-definition".to_owned()),
                            },
                        })
                        .context("failed to plumb context-menu definition lookup")?;
                }
            },
            AcmeContextMenuAction::Cancel => {},
        }
        Ok(())
    }

    fn switch_client_to_search_mode(tab: &mut Tab, client_id: ClientId) -> Result<()> {
        let default_mode = Self::default_client_input_mode(tab);
        let mut mode_info = tab
            .mode_info
            .borrow()
            .get(&client_id)
            .cloned()
            .unwrap_or_else(|| tab.default_mode_info.clone());
        mode_info.mode = InputMode::Search;
        mode_info.base_mode = Some(default_mode);
        tab.change_mode_info(mode_info, client_id);
        tab.mark_active_pane_for_rerender(client_id);
        tab.update_input_modes()?;
        tab.senders
            .send_to_server(ServerInstruction::ChangeMode(
                client_id,
                InputMode::Search,
                None,
            ))
            .context("failed to switch to search mode from context-menu look")
    }

    fn switch_client_to_base_mode(tab: &mut Tab, client_id: ClientId) -> Result<()> {
        let default_mode = Self::default_client_input_mode(tab);
        let configured_base_mode = tab
            .mode_info
            .borrow()
            .get(&client_id)
            .and_then(|mode_info| mode_info.base_mode)
            .unwrap_or(default_mode);
        let (active_pane_is_scrolled, active_pane_is_at_bottom) = tab
            .get_active_pane_or_floating_pane_mut(client_id)
            .map(|pane| {
                let is_scrolled = pane.is_scrolled();
                let is_at_bottom = pane.viewport_is_at_bottom();
                if is_at_bottom {
                    pane.clear_scroll();
                }
                (is_scrolled, is_at_bottom)
            })
            .unwrap_or((false, true));
        let base_mode = if active_pane_is_at_bottom {
            default_mode
        } else if active_pane_is_scrolled {
            InputMode::Scroll
        } else {
            configured_base_mode
        };
        let mut mode_info = tab
            .mode_info
            .borrow()
            .get(&client_id)
            .cloned()
            .unwrap_or_else(|| tab.default_mode_info.clone());
        mode_info.mode = base_mode;
        mode_info.base_mode = Some(default_mode);
        tab.change_mode_info(mode_info, client_id);
        tab.clear_search(client_id);
        tab.mark_active_pane_for_rerender(client_id);
        tab.update_input_modes()?;
        tab.senders
            .send_to_server(ServerInstruction::ChangeMode(client_id, base_mode, None))
            .context("failed to cancel search mode from mouse")
    }

    fn default_client_input_mode(tab: &Tab) -> InputMode {
        tab.default_mode_info
            .base_mode
            .unwrap_or(tab.default_mode_info.mode)
    }

    fn selected_or_word_for_menu(
        tab: &Tab,
        menu: AcmeContextMenuState,
        client_id: ClientId,
    ) -> Option<String> {
        let pane = tab.get_pane_with_id(menu.pane_id)?;
        pane.get_selected_text(client_id)
            .and_then(non_blank_text)
            .or_else(|| {
                let relative_position = pane.relative_position(&menu.anchor_position);
                pane.text_for_word_at(&relative_position)
                    .and_then(non_blank_text)
            })
    }

    fn terminal_pane_id_at_position(tab: &mut Tab, position: Position) -> Result<Option<PaneId>> {
        let Some(pane) = Self::get_pane_at(tab, &position, false)? else {
            return Ok(None);
        };
        let pane_id = pane.pid();
        if pane.position_is_on_frame(&position) || !matches!(pane_id, PaneId::Terminal(_)) {
            return Ok(None);
        }
        Ok(Some(pane_id))
    }

    fn start_pane_resize_with_mouse(
        tab: &mut Tab,
        pane_id: PaneId,
        edge: PaneEdge,
        position: Position,
        _client_id: ClientId,
    ) -> Result<()> {
        let err_context = || format!("failed to start pane resize for pane {pane_id:?}");

        let is_floating = tab.floating_panes.panes_contain(&pane_id);
        if !is_floating {
            tab.repair_native_acme_layout_if_needed()
                .with_context(err_context)?;
        }

        let start_geom = if is_floating {
            tab.floating_panes
                .get_pane(pane_id)
                .map(|p| p.position_and_size())
                .with_context(err_context)?
        } else {
            tab.tiled_panes
                .get_pane(pane_id)
                .map(|p| p.position_and_size())
                .with_context(err_context)?
        };

        let acme_resize_snapshot =
            if !is_floating && matches!(edge, PaneEdge::Top | PaneEdge::Bottom) {
                tab.tiled_panes.acme_resize_snapshot(pane_id)
            } else {
                None
            };

        tab.pane_being_resized_with_mouse = Some(PaneResizeState {
            pane_id,
            edge,
            start_position: position,
            start_geom,
            is_floating,
            acme_resize_snapshot,
        });

        Ok(())
    }

    fn continue_pane_resize_with_mouse(
        tab: &mut Tab,
        current_position: Position,
        _client_id: ClientId,
    ) -> Result<bool> {
        let err_context = || "failed to continue pane resize with mouse";

        let (pane_id, edge, is_floating, acme_resize_snapshot, delta_x, delta_y) =
            if let Some(resize_state) = &tab.pane_being_resized_with_mouse {
                let delta_x = current_position.column() as isize
                    - resize_state.start_position.column() as isize;
                let delta_y = current_position.line() - resize_state.start_position.line();

                if delta_x == 0 && delta_y == 0 && resize_state.acme_resize_snapshot.is_none() {
                    return Ok(false);
                }

                (
                    resize_state.pane_id,
                    resize_state.edge,
                    resize_state.is_floating,
                    resize_state.acme_resize_snapshot.clone(),
                    delta_x,
                    delta_y,
                )
            } else {
                return Ok(true);
            };

        let strategies = edge_and_delta_to_strategies(edge, delta_x, delta_y);
        let is_acme_resize = acme_resize_snapshot.is_some();

        let changed = if is_floating {
            Self::resize_floating_pane_with_strategies(
                tab,
                pane_id,
                &strategies,
                (delta_x.unsigned_abs(), delta_y.unsigned_abs()),
            )
            .with_context(err_context)?;
            true
        } else if let Some(acme_resize_snapshot) = acme_resize_snapshot {
            tab.tiled_panes
                .resize_acme_pane_with_snapshot(
                    pane_id,
                    &acme_resize_snapshot,
                    &strategies,
                    delta_y.unsigned_abs(),
                )
                .with_context(err_context)?
        } else {
            Self::resize_tiled_pane_with_strategies(
                tab,
                pane_id,
                &strategies,
                (delta_x.abs() as f64, delta_y.abs() as f64),
            )
            .with_context(err_context)?;
            true
        };

        if let Some(resize_state) = tab.pane_being_resized_with_mouse.as_mut() {
            if resize_state.acme_resize_snapshot.is_none() {
                resize_state.start_position = current_position;
            }
        }

        if changed {
            tab.set_force_render();
            if is_acme_resize {
                tab.set_should_clear_display_before_rendering();
            }
        }

        Ok(changed)
    }

    fn stop_pane_resize_with_mouse(
        tab: &mut Tab,
        final_position: Position,
        client_id: ClientId,
    ) -> Result<bool> {
        let err_context = || "failed to stop pane resize with mouse";

        let start_geom = tab
            .pane_being_resized_with_mouse
            .as_ref()
            .map(|p| p.start_geom.clone());
        let pane_id = tab
            .pane_being_resized_with_mouse
            .as_ref()
            .map(|p| p.pane_id);
        let _resized = Self::continue_pane_resize_with_mouse(tab, final_position, client_id)
            .with_context(err_context)?;
        let last_geom = pane_id
            .and_then(|pane_id| tab.get_pane_with_id(pane_id))
            .map(|p| p.position_and_size());
        let never_resized = match (start_geom, last_geom) {
            (Some(start_geom), Some(last_geom)) => start_geom == last_geom,
            _ => false,
        };

        tab.pane_being_resized_with_mouse = None;

        Ok(never_resized)
    }

    fn resize_floating_pane_with_strategies(
        tab: &mut Tab,
        pane_id: PaneId,
        strategies: &[ResizeStrategy],
        change_by: (usize, usize),
    ) -> Result<()> {
        let err_context = || format!("failed to resize floating pane {pane_id:?}");

        tab.floating_panes
            .resize_pane_with_strategies(pane_id, strategies, change_by)
            .with_context(err_context)?;

        tab.swap_layouts.set_is_floating_damaged();

        Ok(())
    }

    fn resize_tiled_pane_with_strategies(
        tab: &mut Tab,
        pane_id: PaneId,
        strategies: &[ResizeStrategy],
        change_by: (f64, f64),
    ) -> Result<()> {
        let err_context = || format!("failed to resize tiled pane {pane_id:?}");

        let viewport = tab.viewport.borrow();
        let viewport_cols = viewport.cols;
        let viewport_rows = viewport.rows;

        let change_by_percent = (
            if viewport_cols > 0 {
                (change_by.0 / viewport_cols as f64) * 100.0
            } else {
                0.0
            },
            if viewport_rows > 0 {
                (change_by.1 / viewport_rows as f64) * 100.0
            } else {
                0.0
            },
        );

        tab.tiled_panes
            .resize_pane_with_strategies(pane_id, strategies, change_by_percent)
            .with_context(err_context)?;

        tab.swap_layouts.set_is_tiled_damaged();

        Ok(())
    }

    fn resize_tiled_pane_with_stacked_resize(
        tab: &mut Tab,
        pane_id: PaneId,
        strategy: &ResizeStrategy,
    ) -> Result<()> {
        let err_context = || format!("failed to resize tiled pane {pane_id:?}");

        tab.tiled_panes
            .stacked_resize_pane_with_id(pane_id, strategy, Some((5.0, 5.0)))
            .with_context(err_context)?;

        tab.swap_layouts.set_is_tiled_damaged();

        Ok(())
    }

    fn execute_mouse_action(
        tab: &mut Tab,
        action: MouseAction,
        event: &MouseEvent,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context =
            || format!("failed to execute mouse action {action:?} for client {client_id}");

        let preserves_help_text = matches!(&action, MouseAction::UpdateHover { .. })
            || matches!(
                &action,
                MouseAction::SendToTerminal { event, .. }
                    if event.event_type == MouseEventType::Motion
            );
        if !preserves_help_text {
            tab.mouse_help_text_visible.remove(&client_id);
        }

        match action {
            MouseAction::GroupToggle(pane_id) => {
                if let Some(pane) = tab.get_pane_with_id_mut(pane_id) {
                    let relative_position = pane.relative_position(&event.position);
                    if let Some((hit_plugin_id, pattern, matched_string, context)) =
                        pane.plugin_highlight_at(&relative_position)
                    {
                        let _ = tab
                            .senders
                            .send_to_plugin(PluginInstruction::HighlightClicked {
                                plugin_id: hit_plugin_id,
                                client_id,
                                pane_id,
                                pattern,
                                matched_string,
                                context,
                            });
                        return Ok(MouseEffect::state_changed());
                    }
                }
                Ok(MouseEffect::group_toggle(pane_id))
            },
            MouseAction::GroupAdd(pane_id) => Ok(MouseEffect::group_add(pane_id)),
            MouseAction::Ungroup => Ok(MouseEffect::ungroup()),
            MouseAction::StartResize {
                pane_id,
                edge,
                is_floating: _,
                position,
            } => {
                clear_hover_for_client(tab, client_id);
                Self::start_pane_resize_with_mouse(tab, pane_id, edge, position, client_id)
                    .with_context(err_context)?;
                Ok(MouseEffect::state_changed())
            },
            MouseAction::ContinueResize { position } => {
                let state_changed = Self::continue_pane_resize_with_mouse(tab, position, client_id)
                    .with_context(err_context)?;
                if state_changed {
                    Ok(MouseEffect::state_changed())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::StopResize { position } => {
                Self::execute_stop_resize(tab, position, client_id)
            },
            MouseAction::FocusPane {
                pane_id: _,
                position,
            } => Self::execute_focus_pane(tab, position, client_id),
            MouseAction::StartAcmeHandleDrag { pane_id, position } => {
                clear_hover_for_client(tab, client_id);
                Self::start_acme_handle_drag(tab, pane_id, position, client_id)
                    .with_context(err_context)?;
                Ok(MouseEffect::state_changed())
            },
            MouseAction::ContinueAcmeHandleDrag { position } => {
                Self::continue_acme_handle_drag(tab, position);
                Ok(MouseEffect::default())
            },
            MouseAction::StopAcmeHandleDrag { position } => {
                Self::stop_acme_handle_drag(tab, position, client_id).with_context(err_context)
            },
            MouseAction::StartAcmeContextMenu { pane_id, position } => {
                clear_hover_for_client(tab, client_id);
                tab.set_force_render();
                let menu = AcmeContextMenuState::new(pane_id, position, tab.size);
                tab.acme_context_menus.insert(client_id, menu);
                Ok(MouseEffect::default())
            },
            MouseAction::UpdateAcmeContextMenu { position } => {
                let display_size = tab.size;
                if let Some(menu) = tab.acme_context_menus.get_mut(&client_id) {
                    if menu.update_for_mouse_position(position, display_size) {
                        tab.set_force_render();
                    }
                }
                Ok(MouseEffect::default())
            },
            MouseAction::FinishAcmeContextMenu { position } => {
                let Some(mut menu) = tab.acme_context_menus.remove(&client_id) else {
                    return Ok(MouseEffect::default());
                };
                menu.update_for_mouse_position(position, tab.size);
                let action = menu.action_for_release(position);
                let mut mouse_effect = MouseEffect::default();
                if let Some(action) = action {
                    Self::execute_acme_context_menu_action(tab, menu, action, position, client_id)
                        .with_context(err_context)?;
                    if action == AcmeContextMenuAction::Look {
                        mouse_effect = MouseEffect::state_changed().suppress_scroll_mode_sync();
                    }
                }
                // The menu is drawn outside pane buffers. Force the panes to repaint so the
                // overlay cells are restored even when releasing outside the menu or running an
                // action that does not otherwise change pane contents.
                tab.set_force_render();
                Ok(mouse_effect)
            },
            MouseAction::NewAcmePane { pane_id } => {
                clear_hover_for_client(tab, client_id);
                tab.focus_pane_with_id(pane_id, false, false, client_id)?;
                tab.spawn_acme_pane_for_client(client_id, event.position)?;
                Ok(MouseEffect::state_changed())
            },
            MouseAction::NewAcmeColumn { pane_id } => {
                clear_hover_for_client(tab, client_id);
                tab.focus_pane_with_id(pane_id, false, false, client_id)?;
                tab.spawn_acme_column_for_client(client_id, event.position)?;
                Ok(MouseEffect::state_changed())
            },
            MouseAction::EqualizeAcmePaneRows { pane_id } => {
                let hover_cleared = clear_hover_for_client(tab, client_id);
                if tab.equalize_acme_pane_rows(pane_id, client_id) {
                    tab.move_mouse_from_position_to_pane_title(pane_id, event.position);
                    Ok(MouseEffect::state_changed())
                } else if hover_cleared {
                    Ok(MouseEffect::state_changed())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::EqualizeAcmeColumns { pane_id, edge } => {
                let hover_cleared = clear_hover_for_client(tab, client_id);
                if tab.equalize_acme_columns(client_id) {
                    tab.move_mouse_from_position_to_pane_vertical_border(
                        pane_id,
                        event.position,
                        edge,
                    );
                    Ok(MouseEffect::state_changed())
                } else if hover_cleared {
                    Ok(MouseEffect::state_changed())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::SwapAcmeColumn { pane_id, direction } => {
                clear_hover_for_client(tab, client_id);
                if tab.swap_acme_column(pane_id, direction, client_id) {
                    Ok(MouseEffect::state_changed())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::AcmeClosePane { pane_id } => {
                clear_hover_for_client(tab, client_id);
                tab.close_pane_by_pane_id(pane_id, None)
                    .with_context(err_context)?;
                Ok(MouseEffect::state_changed_and_kill_session_if_no_selectable_panes())
            },
            MouseAction::TogglePaneWrap { pane_id } => {
                clear_hover_for_client(tab, client_id);
                if let Some(pane) = tab.get_pane_with_id_mut(pane_id) {
                    pane.toggle_display_wrap();
                    tab.set_force_render();
                    Ok(MouseEffect::state_changed())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::PasteFromHostClipboard { pane_id } => {
                tab.focus_pane_with_id(pane_id, false, false, client_id)?;
                tab.senders
                    .send_to_screen(ScreenInstruction::PasteFromHostClipboard(pane_id))
                    .with_context(err_context)?;
                Ok(MouseEffect::state_changed())
            },
            MouseAction::PlumbText { pane_id, position } => {
                let Some(pane) = tab.get_pane_with_id(pane_id) else {
                    return Ok(MouseEffect::default());
                };
                let relative_position = pane.relative_position(&position);
                let text = pane
                    .link_uri_at(&relative_position)
                    .map(|uri| TextPlumbPayload {
                        text: uri,
                        click_byte_offset: None,
                        action: None,
                    })
                    .or_else(|| pane.text_for_plumbing_at(&relative_position));
                if let Some(text) = text {
                    tab.senders
                        .send_to_pty(PtyInstruction::PlumbText { pane_id, text })
                        .with_context(err_context)?;
                }
                Ok(MouseEffect::default())
            },
            MouseAction::FocusPaneAndClickThrough {
                pane_id: _,
                position,
                event: click_event,
            } => Self::execute_focus_pane_and_click_through(tab, position, click_event, client_id),
            MouseAction::ShowFloatingPanesAndFocus { pane_id } => {
                tab.show_floating_panes();
                tab.floating_panes.focus_pane(pane_id, client_id);
                Ok(MouseEffect::state_changed())
            },
            MouseAction::StartSelection { pane_id, position } => {
                let osc133_command_selection = tab.osc133_command_selection;
                let word_separators = tab.word_separators.clone();
                let pane = tab
                    .get_pane_with_id_mut(pane_id)
                    .ok_or_else(|| anyhow!("Failed to find pane {pane_id:?}"))?;
                let relative_position = pane.relative_position(&position);

                let mut leave_clipboard_message = false;
                pane.set_selection_options(osc133_command_selection, &word_separators);
                pane.start_selection(&relative_position, client_id);
                if pane.get_selected_text(client_id).is_some() {
                    leave_clipboard_message = true;
                }
                if pane.supports_mouse_selection() {
                    tab.selecting_with_mouse_in_pane = Some(pane_id);
                }
                if leave_clipboard_message {
                    Ok(MouseEffect::state_changed_and_leave_clipboard_message())
                } else {
                    Ok(MouseEffect::default())
                }
            },
            MouseAction::UpdateSelection { position } => {
                if let Some(pane_id_with_selection) = tab.selecting_with_mouse_in_pane {
                    if let Some(pane_with_selection) =
                        tab.get_pane_with_id_mut(pane_id_with_selection)
                    {
                        let relative_position = pane_with_selection.relative_position(&position);
                        pane_with_selection.update_selection(&relative_position, client_id);
                    }
                }
                Ok(MouseEffect::default())
            },
            MouseAction::EndSelection { position } => {
                Self::execute_end_selection(tab, position, client_id)
            },
            MouseAction::StartMovingFloatingPane { position } => {
                Self::execute_move_floating_pane(tab, position)
            },
            MouseAction::ContinueMovingFloatingPane { position } => {
                Self::execute_move_floating_pane(tab, position)
            },
            MouseAction::StopMovingFloatingPane { position } => {
                Self::execute_stop_moving_floating_pane(tab, position, client_id)
            },
            MouseAction::ScrollUp { pane_id: _, lines } => {
                Self::handle_scrollwheel_up(tab, &event.position, lines, client_id)
                    .with_context(err_context)
            },
            MouseAction::ScrollDown { pane_id: _, lines } => {
                Self::handle_scrollwheel_down(tab, &event.position, lines, client_id)
                    .with_context(err_context)
            },
            MouseAction::ScrollLeft { pane_id, cols } => {
                let scroll_right = false;
                Self::handle_scrollwheel_horizontal(
                    tab,
                    pane_id,
                    &event.position,
                    cols,
                    scroll_right,
                    client_id,
                )
                .with_context(err_context)
            },
            MouseAction::ScrollRight { pane_id, cols } => {
                let scroll_right = true;
                Self::handle_scrollwheel_horizontal(
                    tab,
                    pane_id,
                    &event.position,
                    cols,
                    scroll_right,
                    client_id,
                )
                .with_context(err_context)
            },
            MouseAction::ResizeScrollUp { pane_id } => {
                Self::handle_resize_scroll_up(tab, pane_id, client_id).with_context(err_context)
            },
            MouseAction::ResizeScrollDown { pane_id } => {
                Self::handle_resize_scroll_down(tab, pane_id, client_id).with_context(err_context)
            },
            MouseAction::ScrollToPreviousPrompt { pane_id } => {
                Self::handle_prompt_jump(tab, pane_id, true, event, client_id)
            },
            MouseAction::ScrollToNextPrompt { pane_id } => {
                Self::handle_prompt_jump(tab, pane_id, false, event, client_id)
            },
            MouseAction::UpdateHover { pane_id, position } => {
                Self::execute_update_hover(tab, pane_id, position, client_id)
            },
            MouseAction::FocusOnHover { pane_id, position } => {
                Self::execute_focus_on_hover(tab, pane_id, position, client_id)
            },
            MouseAction::SearchDown => {
                tab.search_down(client_id);
                Ok(MouseEffect::state_changed().suppress_scroll_mode_sync())
            },
            MouseAction::SearchUp => {
                tab.search_up(client_id);
                Ok(MouseEffect::state_changed().suppress_scroll_mode_sync())
            },
            MouseAction::CancelSearch => {
                Self::switch_client_to_base_mode(tab, client_id)?;
                Ok(MouseEffect::state_changed().suppress_scroll_mode_sync())
            },
            MouseAction::SendToTerminal { pane_id, event } => {
                Self::execute_send_to_terminal(tab, pane_id, event, client_id)
            },
            MouseAction::FrameIntercepted { pane_id: _ } => {
                tab.set_force_render();
                Ok(MouseEffect::state_changed())
            },
            MouseAction::NoAction => Ok(MouseEffect::default()),
        }
    }

    fn execute_stop_resize(
        tab: &mut Tab,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || "failed to stop resize";
        let never_resized = Self::stop_pane_resize_with_mouse(tab, position, client_id)
            .with_context(err_context)?;
        if never_resized {
            let pane_id_at_position = Self::get_pane_at(tab, &position, false)
                .with_context(err_context)?
                .map(|p| p.pid());
            let active_pane_id = tab
                .get_active_pane_id(client_id)
                .ok_or_else(|| anyhow!("Failed to find active pane"))?;
            if let Some(pane_id) = pane_id_at_position {
                if pane_id != active_pane_id {
                    Self::focus_pane_at(tab, &position, client_id).with_context(err_context)?;
                }
            }
        }
        Ok(MouseEffect::state_changed())
    }

    fn execute_focus_pane(
        tab: &mut Tab,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || "failed to focus pane";
        clear_hover_for_client(tab, client_id);
        let active_pane_id_before = tab
            .get_active_pane_id(client_id)
            .ok_or_else(|| anyhow!("Failed to find active pane"))?;

        Self::focus_pane_at(tab, &position, client_id).with_context(err_context)?;

        let osc133_command_selection = tab.osc133_command_selection;
        let word_separators = tab.word_separators.clone();
        if let Some(pane_at_position) = Self::unselectable_pane_at_position(tab, &position) {
            let relative_position = pane_at_position.relative_position(&position);
            pane_at_position.set_selection_options(osc133_command_selection, &word_separators);
            pane_at_position.start_selection(&relative_position, client_id);
        }

        if tab.floating_panes.panes_are_visible() {
            let search_selectable = false;
            let moved_pane_with_mouse = tab
                .floating_panes
                .move_pane_with_mouse(position, search_selectable);
            let active_pane_id_after = tab
                .get_active_pane_id(client_id)
                .ok_or_else(|| anyhow!("Failed to find active pane"))?;
            if moved_pane_with_mouse || active_pane_id_before != active_pane_id_after {
                return Ok(MouseEffect::state_changed());
            } else {
                return Ok(MouseEffect::default());
            }
        }

        let active_pane_id_after = tab
            .get_active_pane_id(client_id)
            .ok_or_else(|| anyhow!("Failed to find active pane"))?;
        if active_pane_id_before != active_pane_id_after {
            Ok(MouseEffect::state_changed())
        } else {
            Ok(MouseEffect::default())
        }
    }

    fn execute_focus_pane_and_click_through(
        tab: &mut Tab,
        position: Position,
        click_event: MouseEvent,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || "failed to focus pane and click through";

        clear_hover_for_client(tab, client_id);
        Self::focus_pane_at(tab, &position, client_id).with_context(err_context)?;

        let osc133_command_selection = tab.osc133_command_selection;
        let word_separators = tab.word_separators.clone();
        if let Some(pane_at_position) = Self::unselectable_pane_at_position(tab, &position) {
            let relative_position = pane_at_position.relative_position(&position);
            pane_at_position.set_selection_options(osc133_command_selection, &word_separators);
            pane_at_position.start_selection(&relative_position, client_id);
            return Ok(MouseEffect::state_changed());
        }

        let active_pane_id = tab
            .get_active_pane_id(client_id)
            .ok_or_else(|| anyhow!("Failed to find active pane"))
            .with_context(err_context)?;

        let pane = tab
            .get_pane_with_id(active_pane_id)
            .ok_or_else(|| anyhow!("Failed to find pane {active_pane_id:?}"))
            .with_context(err_context)?;

        let terminal_wants_mouse = pane.terminal_emulator_wants_mouse();

        if terminal_wants_mouse {
            let relative_position = pane.relative_position(&click_event.position);
            let mut event_for_pane = click_event;
            event_for_pane.position = relative_position;
            if let Some(mouse_event) = pane.mouse_event(&event_for_pane, client_id) {
                if !pane.position_is_on_frame(&click_event.position) {
                    tab.write_to_active_terminal(&None, mouse_event.into_bytes(), false, client_id)
                        .with_context(err_context)?;
                }
            }
        } else {
            if let Some(pane) = tab.get_pane_with_id_mut(active_pane_id) {
                let relative_position = pane.relative_position(&position);
                pane.set_selection_options(osc133_command_selection, &word_separators);
                pane.start_selection(&relative_position, client_id);
                if pane.supports_mouse_selection() {
                    tab.selecting_with_mouse_in_pane = Some(active_pane_id);
                }
            }
        }

        Ok(MouseEffect::state_changed())
    }

    fn execute_end_selection(
        tab: &mut Tab,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || "failed to end selection";
        let mut leave_clipboard_message = false;
        let copy_on_release = tab.copy_on_select;

        if let Some(pane_with_selection) = tab
            .selecting_with_mouse_in_pane
            .and_then(|p_id| tab.get_pane_with_id_mut(p_id))
        {
            let mut relative_position = pane_with_selection.relative_position(&position);

            relative_position.change_column(
                (relative_position.column())
                    .max(0)
                    .min(pane_with_selection.get_content_columns()),
            );

            relative_position.change_line(
                (relative_position.line())
                    .max(0)
                    .min(pane_with_selection.get_content_rows() as isize),
            );

            if let Some(mouse_event) =
                pane_with_selection.mouse_left_click_release(&relative_position)
            {
                tab.write_to_active_terminal(&None, mouse_event.into_bytes(), false, client_id)
                    .with_context(err_context)?;
            } else {
                let relative_position = pane_with_selection.relative_position(&position);
                pane_with_selection.end_selection(&relative_position, client_id);
                if pane_with_selection.supports_mouse_selection() {
                    if copy_on_release {
                        let selected_text = pane_with_selection.get_selected_text(client_id);
                        if let Some(selected_text) = selected_text {
                            leave_clipboard_message = true;
                            tab.write_selection_to_clipboard(&selected_text)
                                .with_context(err_context)?;
                        }
                    }
                }
                tab.selecting_with_mouse_in_pane = None;
            }
        }

        if leave_clipboard_message {
            Ok(MouseEffect::leave_clipboard_message())
        } else {
            Ok(MouseEffect::default())
        }
    }

    fn execute_move_floating_pane(tab: &mut Tab, position: Position) -> Result<MouseEffect> {
        let search_selectable = false;
        if tab
            .floating_panes
            .move_pane_with_mouse(position, search_selectable)
        {
            tab.swap_layouts.set_is_floating_damaged();
            tab.set_force_render();
            Ok(MouseEffect::state_changed())
        } else {
            Ok(MouseEffect::default())
        }
    }

    fn execute_stop_moving_floating_pane(
        tab: &mut Tab,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || "failed to stop moving floating pane";
        let never_moved = tab.floating_panes.stop_moving_pane_with_mouse(position);
        if never_moved {
            let active_pane_id = tab
                .get_active_pane_id(client_id)
                .ok_or_else(|| anyhow!("Failed to find active pane"))?;
            let pane_id_at_position = Self::get_pane_at(tab, &position, false)
                .with_context(err_context)?
                .ok_or_else(|| anyhow!("Failed to find pane at position"))?
                .pid();
            if active_pane_id != pane_id_at_position {
                Self::focus_pane_at(tab, &position, client_id).with_context(err_context)?;
            }
        }
        Ok(MouseEffect::default())
    }

    fn execute_focus_on_hover(
        tab: &mut Tab,
        pane_id: PaneId,
        position: Position,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || format!("failed to focus pane on hover for client {client_id}");

        let is_selectable = tab
            .get_pane_with_id(pane_id)
            .map(|p| p.selectable())
            .unwrap_or(false);
        if !is_selectable {
            return Self::execute_update_hover(tab, Some(pane_id), Some(position), client_id);
        }

        let floating_visible = tab.floating_panes.panes_are_visible();
        let is_floating = tab.floating_panes.get_pane(pane_id).is_some();
        if floating_visible && !is_floating {
            return Self::execute_update_hover(tab, Some(pane_id), Some(position), client_id);
        }

        let is_stacked_one_liner = tab
            .get_pane_with_id(pane_id)
            .map(|p| {
                let geom = p.current_geom();
                geom.is_stacked() && geom.rows.is_fixed()
            })
            .unwrap_or(false);
        if is_stacked_one_liner {
            return Self::execute_update_hover(tab, Some(pane_id), Some(position), client_id);
        }

        if tab.pane_is_hidden_stack_list_member(&pane_id) {
            return Self::execute_update_hover(tab, Some(pane_id), Some(position), client_id);
        }

        let active_pane_id = tab.get_active_pane_id(client_id);
        if active_pane_id == Some(pane_id) {
            return Self::execute_update_hover(tab, Some(pane_id), Some(position), client_id);
        }

        Self::focus_pane_at(tab, &position, client_id).with_context(err_context)?;

        clear_hover_for_client(tab, client_id);

        Ok(MouseEffect::state_changed())
    }

    fn execute_update_hover(
        tab: &mut Tab,
        pane_id: Option<PaneId>,
        position: Option<Position>,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let mut should_render = false;
        let previous_hover_pane_id = tab.mouse_hover_pane_id.get(&client_id).copied();
        if tab.mouse_hover_effects {
            let previous_plugin_hover_pane_id = tab.plugin_hover_pane_id.get(&client_id).copied();
            let current_plugin_hover_pane_id = match pane_id {
                Some(pid) if matches!(pid, PaneId::Plugin(_)) => Some(pid),
                _ => None,
            };
            if let (Some(pid), Some(position)) = (current_plugin_hover_pane_id, position) {
                if let Some(pane) = tab.get_pane_with_id(pid) {
                    let relative_position = pane.relative_position(&position);
                    let _ = pane.mouse_event(
                        &MouseEvent::new_buttonless_motion(relative_position),
                        client_id,
                    );
                }
            }
            if previous_plugin_hover_pane_id != current_plugin_hover_pane_id {
                if let Some(previous_pid) = previous_plugin_hover_pane_id {
                    if let Some(pane) = tab.get_pane_with_id(previous_pid) {
                        let _ = pane.mouse_event(
                            &MouseEvent::new_buttonless_motion(Position::new(0, u16::MAX)),
                            client_id,
                        );
                    }
                }
                match current_plugin_hover_pane_id {
                    Some(pid) => {
                        tab.plugin_hover_pane_id.insert(client_id, pid);
                    },
                    None => {
                        tab.plugin_hover_pane_id.remove(&client_id);
                    },
                }
            }
        }
        match pane_id {
            Some(pid) => {
                if let Some(pane) = tab.get_pane_with_id(pid) {
                    let pane_is_selectable = pane.selectable();
                    if tab.advanced_mouse_actions && tab.mouse_hover_effects && pane_is_selectable {
                        tab.mouse_hover_pane_id.insert(client_id, pid);
                    } else if tab.advanced_mouse_actions || !tab.mouse_hover_effects {
                        tab.mouse_hover_pane_id.remove(&client_id);
                    }
                    tab.mouse_last_pane_id.insert(client_id, pid);
                    should_render = true;
                }
            },
            None => {
                tab.mouse_last_pane_id.remove(&client_id);
                let removed = tab.mouse_hover_pane_id.remove(&client_id);
                if removed.is_some() {
                    should_render = true;
                }
            },
        }

        if let Some(prev_pane_id) = previous_hover_pane_id {
            if Some(prev_pane_id) != pane_id {
                if let Some(pane) = tab.get_pane_with_id_mut(prev_pane_id) {
                    pane.set_hover_position(None);
                }
            }
        }

        if tab.mouse_help_text_visible.remove(&client_id).is_some() {
            should_render = true;
        }

        let mut mouse_effect = if should_render {
            MouseEffect::state_changed()
        } else {
            MouseEffect::default()
        };
        mouse_effect.leave_clipboard_message = true;
        Ok(mouse_effect)
    }

    fn execute_send_to_terminal(
        tab: &mut Tab,
        pane_id: PaneId,
        event: MouseEvent,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || format!("failed to send to terminal for pane {pane_id:?}");
        let mut should_render = false;
        let active_pane_id = tab
            .get_active_pane_id(client_id)
            .ok_or_else(|| anyhow!("Failed to find active pane"))?;
        if pane_id == active_pane_id {
            let pane = tab
                .get_pane_with_id(pane_id)
                .ok_or_else(|| anyhow!("Failed to find pane {pane_id:?}"))?;
            let relative_position = pane.relative_position(&event.position);
            let mut event_for_pane = event.clone();
            event_for_pane.position = relative_position;
            if let Some(mouse_event) = pane.mouse_event(&event_for_pane, client_id) {
                if !pane.position_is_on_frame(&event.position) {
                    tab.write_to_active_terminal(&None, mouse_event.into_bytes(), false, client_id)
                        .with_context(err_context)?;
                }
            }
            if clear_hover_for_client(tab, client_id) {
                should_render = true;
            }
            if event.event_type == MouseEventType::Motion {
                if let Some(pane) = tab.get_pane_with_id_mut(pane_id) {
                    if !pane.terminal_emulator_wants_mouse() {
                        let relative = pane.relative_position(&event.position);
                        if pane.set_hover_position(Some(relative)) {
                            should_render = true;
                        }
                    }
                }
            }

            if event.event_type == MouseEventType::Motion && tab.mouse_hover_effects {
                tab.last_mouse_activity_time
                    .insert(client_id, Instant::now());
                let entered_pane = tab.mouse_last_pane_id.get(&client_id) != Some(&pane_id);
                tab.mouse_last_pane_id.insert(client_id, pane_id);
                if entered_pane && tab.mouse_hover_tips {
                    let was_visible = tab
                        .mouse_help_text_visible
                        .get(&client_id)
                        .copied()
                        .unwrap_or(false);
                    tab.mouse_help_text_visible.insert(client_id, true);
                    if !was_visible {
                        should_render = true;
                    }

                    tab.senders
                        .send_to_background_jobs(BackgroundJob::ClearHelpText { client_id })
                        .with_context(err_context)?;
                }
            }
        }
        let mouse_effect = if should_render {
            MouseEffect::state_changed()
        } else {
            MouseEffect::default()
        };
        Ok(mouse_effect)
    }

    fn determine_mouse_action(event: &MouseEvent, ctx: &MouseEventContext) -> Result<MouseAction> {
        if ctx.acme_context_menu.is_some() {
            return Ok(match event.event_type {
                MouseEventType::Motion => MouseAction::UpdateAcmeContextMenu {
                    position: event.position,
                },
                MouseEventType::Release => MouseAction::FinishAcmeContextMenu {
                    position: event.position,
                },
                _ => MouseAction::NoAction,
            });
        }

        if ctx.pane_being_resized {
            return Ok(match event.event_type {
                MouseEventType::Motion => MouseAction::ContinueResize {
                    position: event.position,
                },
                MouseEventType::Release => MouseAction::StopResize {
                    position: event.position,
                },
                _ => MouseAction::NoAction,
            });
        }

        if ctx.selecting_with_mouse {
            return Ok(match event.event_type {
                MouseEventType::Motion if event.left => MouseAction::UpdateSelection {
                    position: event.position,
                },
                MouseEventType::Release if event.left => MouseAction::EndSelection {
                    position: event.position,
                },
                _ => MouseAction::NoAction,
            });
        }

        if ctx.pane_being_moved {
            return Ok(match event.event_type {
                MouseEventType::Motion if event.left => MouseAction::ContinueMovingFloatingPane {
                    position: event.position,
                },
                MouseEventType::Release if event.left => MouseAction::StopMovingFloatingPane {
                    position: event.position,
                },
                _ => MouseAction::NoAction,
            });
        }

        if ctx.acme_handle_drag.is_some() {
            return Ok(match event.event_type {
                MouseEventType::Motion => MouseAction::ContinueAcmeHandleDrag {
                    position: event.position,
                },
                MouseEventType::Release => MouseAction::StopAcmeHandleDrag {
                    position: event.position,
                },
                _ => MouseAction::NoAction,
            });
        }

        if ctx.input_mode == InputMode::Search && event.event_type == MouseEventType::Press {
            if let Some(details) = &ctx.clicked_pane {
                if !details.on_frame && matches!(details.pane_id, PaneId::Terminal(_)) {
                    if event.left {
                        return Ok(MouseAction::SearchDown);
                    }
                    if event.right {
                        return Ok(MouseAction::SearchUp);
                    }
                    if event.middle {
                        return Ok(MouseAction::CancelSearch);
                    }
                }
            }
        }

        if event.right && event.event_type == MouseEventType::Press {
            if let Some(details) = &ctx.clicked_pane {
                if !details.on_frame && matches!(details.pane_id, PaneId::Terminal(_)) {
                    return Ok(MouseAction::StartAcmeContextMenu {
                        pane_id: details.pane_id,
                        position: event.position,
                    });
                }
            }
        }

        if event.alt {
            if let (Some(passthrough_pane_id), Some(details)) =
                (ctx.passthrough_pane_id, ctx.clicked_pane.as_ref())
            {
                if details.pane_id == passthrough_pane_id
                    && !details.on_frame
                    && details.terminal_wants_mouse
                {
                    return Ok(MouseAction::SendToTerminal {
                        pane_id: details.pane_id,
                        event: *event,
                    });
                }
            }

            if event.wheel_up || event.wheel_down {
                if !ctx.advanced_mouse_actions {
                    return Ok(MouseAction::NoAction);
                }
                if let Some(pane_id) = ctx.pane_id_at_position {
                    if event.wheel_up {
                        return Ok(MouseAction::ScrollToPreviousPrompt { pane_id });
                    }
                    return Ok(MouseAction::ScrollToNextPrompt { pane_id });
                }
                return Ok(MouseAction::NoAction);
            }

            if let Some(pane_id) = ctx.acme_title_button_pane_id {
                if event.event_type == MouseEventType::Press && event.left {
                    return Ok(MouseAction::SwapAcmeColumn {
                        pane_id,
                        direction: Direction::Left,
                    });
                }
                if event.event_type == MouseEventType::Press && event.right {
                    return Ok(MouseAction::SwapAcmeColumn {
                        pane_id,
                        direction: Direction::Right,
                    });
                }
                if event.left || event.right {
                    return Ok(MouseAction::NoAction);
                }
            }
            let is_left_press = event.left && event.event_type == MouseEventType::Press;
            let is_left_motion = event.left && event.event_type == MouseEventType::Motion;

            if is_left_press {
                if let Some(pane_id) = ctx.pane_id_at_position {
                    return Ok(MouseAction::GroupToggle(pane_id));
                }
            }
            if is_left_motion {
                if let Some(pane_id) = ctx.pane_id_at_position {
                    return Ok(MouseAction::GroupAdd(pane_id));
                }
            }
            if event.right && event.event_type == MouseEventType::Press {
                return Ok(MouseAction::Ungroup);
            }
            return Ok(MouseAction::NoAction);
        }

        if event.wheel_up || event.wheel_down {
            if event.ctrl && !ctx.mouse_scroll_resize {
                return Ok(MouseAction::NoAction);
            }
            if let Some(pane_id) = ctx.pane_id_at_position {
                if event.ctrl {
                    if event.wheel_up {
                        return Ok(MouseAction::ResizeScrollUp { pane_id });
                    }
                    if event.wheel_down {
                        return Ok(MouseAction::ResizeScrollDown { pane_id });
                    }
                }
                if event.wheel_up {
                    return Ok(MouseAction::ScrollUp { pane_id, lines: 3 });
                }
                if event.wheel_down {
                    return Ok(MouseAction::ScrollDown { pane_id, lines: 3 });
                }
            }
            return Ok(MouseAction::NoAction);
        }

        if event.wheel_left || event.wheel_right {
            if let Some(pane_id) = ctx.pane_id_at_position {
                if event.wheel_left {
                    return Ok(MouseAction::ScrollLeft { pane_id, cols: 4 });
                }
                if event.wheel_right {
                    return Ok(MouseAction::ScrollRight { pane_id, cols: 4 });
                }
            }
            return Ok(MouseAction::NoAction);
        }

        let is_ctrl_right_press =
            event.ctrl && event.right && event.event_type == MouseEventType::Press;
        if is_ctrl_right_press {
            if let Some(pane_id) = ctx.acme_title_button_pane_id {
                return Ok(MouseAction::NewAcmeColumn { pane_id });
            }
            return Ok(MouseAction::NoAction);
        }

        let is_ctrl_left_press =
            event.ctrl && event.left && event.event_type == MouseEventType::Press;
        if is_ctrl_left_press {
            if let Some(pane_id) = ctx.acme_title_button_pane_id {
                return Ok(MouseAction::NewAcmePane { pane_id });
            }
            if let Some(pane_id) = ctx.acme_title_pane_id {
                return Ok(MouseAction::EqualizeAcmePaneRows { pane_id });
            }
            let Some(details) = &ctx.clicked_pane else {
                return Ok(MouseAction::NoAction);
            };
            if details.on_frame {
                if ctx.acme_vertical_border_hit {
                    if let Some(edge) = details.edge {
                        return Ok(MouseAction::EqualizeAcmeColumns {
                            pane_id: details.pane_id,
                            edge,
                        });
                    }
                    return Ok(MouseAction::NoAction);
                }
                if details.frame_intercepted {
                    return Ok(MouseAction::FrameIntercepted {
                        pane_id: details.pane_id,
                    });
                }
                if let Some(edge) = details.edge {
                    return Ok(MouseAction::StartResize {
                        pane_id: details.pane_id,
                        edge,
                        is_floating: details.is_floating,
                        position: event.position,
                    });
                }
                return Ok(MouseAction::NoAction);
            }
            if matches!(details.pane_id, PaneId::Terminal(_)) {
                return Ok(MouseAction::PlumbText {
                    pane_id: details.pane_id,
                    position: event.position,
                });
            }
            return Ok(MouseAction::NoAction);
        }

        let is_plain_left_press =
            event.left && event.event_type == MouseEventType::Press && !event.ctrl && !event.alt;
        if is_plain_left_press {
            let Some(details) = &ctx.clicked_pane else {
                return Ok(MouseAction::NoAction);
            };

            let is_active_pane = Some(details.pane_id) == ctx.active_pane_id;
            let is_pinned_pane = ctx
                .pinned_selectable
                .map(|id| id == details.pane_id)
                .unwrap_or(false);

            if ctx.acme_wrap_indicator_pane_id == Some(details.pane_id) {
                return Ok(MouseAction::TogglePaneWrap {
                    pane_id: details.pane_id,
                });
            }

            if ctx.acme_title_button_pane_id == Some(details.pane_id) {
                return Ok(MouseAction::StartAcmeHandleDrag {
                    pane_id: details.pane_id,
                    position: event.position,
                });
            }

            if details.on_frame {
                if details.frame_intercepted {
                    return Ok(MouseAction::FrameIntercepted {
                        pane_id: details.pane_id,
                    });
                }

                let should_start_moving = ctx.floating_visible || is_pinned_pane;
                if should_start_moving {
                    return Ok(MouseAction::StartMovingFloatingPane {
                        position: event.position,
                    });
                }

                if let Some(edge) = details.edge {
                    return Ok(MouseAction::StartResize {
                        pane_id: details.pane_id,
                        edge,
                        is_floating: false,
                        position: event.position,
                    });
                }
                if details.is_acme_title {
                    return Ok(MouseAction::NoAction);
                }
            }

            if is_active_pane {
                if details.terminal_wants_mouse {
                    return Ok(MouseAction::SendToTerminal {
                        pane_id: details.pane_id,
                        event: *event,
                    });
                } else {
                    return Ok(MouseAction::StartSelection {
                        pane_id: details.pane_id,
                        position: event.position,
                    });
                }
            }

            if !ctx.floating_visible {
                if let Some(pinned_id) = ctx.pinned_selectable {
                    return Ok(MouseAction::ShowFloatingPanesAndFocus { pane_id: pinned_id });
                }
                if ctx.pinned_unselectable.is_some() {
                    return Ok(MouseAction::NoAction);
                }
            }

            if ctx.mouse_click_through && !ctx.focus_follows_mouse {
                return Ok(MouseAction::FocusPaneAndClickThrough {
                    pane_id: details.pane_id,
                    position: event.position,
                    event: *event,
                });
            } else {
                return Ok(MouseAction::FocusPane {
                    pane_id: details.pane_id,
                    position: event.position,
                });
            }
        }

        if event.right {
            return Ok(MouseAction::NoAction);
        }

        if event.middle {
            if event.event_type == MouseEventType::Press {
                if let Some(pane_id) = ctx.acme_title_button_pane_id {
                    return Ok(MouseAction::AcmeClosePane { pane_id });
                }
            }
            let Some(details) = &ctx.clicked_pane else {
                return Ok(MouseAction::NoAction);
            };
            let is_active_pane = Some(details.pane_id) == ctx.active_pane_id;
            if details.terminal_wants_mouse {
                if is_active_pane {
                    return Ok(MouseAction::SendToTerminal {
                        pane_id: details.pane_id,
                        event: *event,
                    });
                }
            } else if event.event_type == MouseEventType::Press
                && matches!(details.pane_id, PaneId::Terminal(_))
            {
                return Ok(MouseAction::PasteFromHostClipboard {
                    pane_id: details.pane_id,
                });
            }
            return Ok(MouseAction::NoAction);
        }

        let is_left_motion_or_release = event.left
            && (event.event_type == MouseEventType::Motion
                || event.event_type == MouseEventType::Release);
        if is_left_motion_or_release {
            let Some(details) = &ctx.clicked_pane else {
                return Ok(MouseAction::NoAction);
            };
            let is_active_pane = Some(details.pane_id) == ctx.active_pane_id;
            if is_active_pane && details.terminal_wants_mouse {
                return Ok(MouseAction::SendToTerminal {
                    pane_id: details.pane_id,
                    event: *event,
                });
            }
            return Ok(MouseAction::NoAction);
        }

        let is_buttonless_motion = event.event_type == MouseEventType::Motion
            && !event.left
            && !event.right
            && !event.middle;
        if is_buttonless_motion {
            if ctx.acme_title_pane_id.is_some() {
                return Ok(MouseAction::UpdateHover {
                    pane_id: None,
                    position: None,
                });
            }
            let Some(pane_id) = ctx.pane_id_at_position else {
                return Ok(MouseAction::UpdateHover {
                    pane_id: None,
                    position: None,
                });
            };
            let is_active_pane = Some(pane_id) == ctx.active_pane_id;
            if is_active_pane {
                return Ok(MouseAction::SendToTerminal {
                    pane_id,
                    event: *event,
                });
            }
            if ctx.focus_follows_mouse {
                return Ok(MouseAction::FocusOnHover {
                    pane_id,
                    position: event.position,
                });
            }
            return Ok(MouseAction::UpdateHover {
                pane_id: Some(pane_id),
                position: Some(event.position),
            });
        }

        Ok(MouseAction::NoAction)
    }

    fn unselectable_pane_at_position<'a>(
        tab: &'a mut Tab,
        point: &Position,
    ) -> Option<&'a mut Box<dyn Pane>> {
        // the repetition in this function is to appease the borrow checker, I don't like it either
        let floating_panes_are_visible = tab.floating_panes.panes_are_visible();
        if floating_panes_are_visible {
            if let Ok(Some(clicked_pane_id)) = tab.floating_panes.get_pane_id_at(point, true) {
                if let Some(pane) = tab.floating_panes.get_pane_mut(clicked_pane_id) {
                    if !pane.selectable() {
                        return Some(pane);
                    }
                }
            } else if let Ok(Some(clicked_pane_id)) = tab.get_pane_id_at(point, false) {
                if let Some(pane) = tab.tiled_panes.get_pane_mut(clicked_pane_id) {
                    if !pane.selectable() {
                        return Some(pane);
                    }
                }
            }
        } else if let Ok(Some(clicked_pane_id)) = tab.get_pane_id_at(point, false) {
            if let Some(pane) = tab.tiled_panes.get_pane_mut(clicked_pane_id) {
                if !pane.selectable() {
                    return Some(pane);
                }
            }
        }
        None
    }

    fn focus_pane_at(tab: &mut Tab, point: &Position, client_id: ClientId) -> Result<()> {
        let err_context =
            || format!("failed to focus pane at position {point:?} for client {client_id}");

        if tab.floating_panes.panes_are_visible() {
            if let Some(clicked_pane) = tab
                .floating_panes
                .get_pane_id_at(point, true)
                .with_context(err_context)?
            {
                tab.floating_panes.focus_pane(clicked_pane, client_id);
                tab.set_pane_active_at(clicked_pane);
                return Ok(());
            }
        }
        if tab.floating_panes.has_pinned_panes() {
            let search_selectable = false;
            if let Some(pane_id) = tab
                .floating_panes
                .get_pinned_pane_id_at(point, search_selectable)
                .with_context(err_context)?
            {
                tab.floating_panes.focus_pane(pane_id, client_id);
                tab.set_pane_active_at(pane_id);
                tab.show_floating_panes();
                return Ok(());
            }
        }
        if let Some(clicked_pane) = tab.get_pane_id_at(point, true).with_context(err_context)? {
            if !tab.focus_hidden_stack_list_member(clicked_pane, client_id) {
                tab.tiled_panes.focus_pane(clicked_pane, client_id);
            }
            tab.set_pane_active_at(clicked_pane);
            if tab.floating_panes.panes_are_visible() {
                tab.hide_floating_panes();
                tab.set_force_render();
            }
        }
        Ok(())
    }

    pub(crate) fn handle_scrollwheel_up(
        tab: &mut Tab,
        point: &Position,
        lines: usize,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || {
            format!("failed to handle scrollwheel up at position {point:?} for client {client_id}")
        };

        if let Some(pane) = Self::get_pane_at(tab, point, false).with_context(err_context)? {
            let relative_position = pane.relative_position(point);
            if let Some(mouse_event) = pane.mouse_scroll_up(&relative_position) {
                tab.write_to_terminal_at(mouse_event.into_bytes(), point, client_id)
                    .with_context(err_context)?;
            } else if pane.is_alternate_mode_active() {
                // separate writes so each sequence gets adjusted for cursor keys mode
                for _ in 0..lines {
                    tab.write_to_terminal_at("\u{1b}[A".as_bytes().to_owned(), point, client_id)
                        .with_context(err_context)?;
                }
            } else {
                pane.scroll_up(lines, client_id);
            }
        }
        Ok(MouseEffect::default())
    }

    pub(crate) fn handle_scrollwheel_down(
        tab: &mut Tab,
        point: &Position,
        lines: usize,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || {
            format!(
                "failed to handle scrollwheel down at position {point:?} for client {client_id}"
            )
        };

        if let Some(pane) = Self::get_pane_at(tab, point, false).with_context(err_context)? {
            let relative_position = pane.relative_position(point);
            if let Some(mouse_event) = pane.mouse_scroll_down(&relative_position) {
                tab.write_to_terminal_at(mouse_event.into_bytes(), point, client_id)
                    .with_context(err_context)?;
            } else if pane.is_alternate_mode_active() {
                // separate writes so each sequence gets adjusted for cursor keys mode
                for _ in 0..lines {
                    tab.write_to_terminal_at("\u{1b}[B".as_bytes().to_owned(), point, client_id)
                        .with_context(err_context)?;
                }
            } else {
                pane.scroll_down(lines, client_id);
                if !pane.is_scrolled() {
                    if let PaneId::Terminal(pid) = pane.pid() {
                        tab.process_pending_vte_events(pid)
                            .with_context(err_context)?;
                    }
                }
            }
        }
        Ok(MouseEffect::default())
    }

    fn handle_prompt_jump(
        tab: &mut Tab,
        pane_id: PaneId,
        to_previous_prompt: bool,
        event: &MouseEvent,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context =
            || format!("failed to jump to prompt in pane {pane_id:?} for client {client_id}");

        let report_for_pane = tab.get_pane_with_id(pane_id).and_then(|pane| {
            let mut event_for_pane = *event;
            event_for_pane.position = pane.relative_position(&event.position);
            pane.mouse_event(&event_for_pane, client_id)
        });
        if let Some(report_for_pane) = report_for_pane {
            tab.write_to_terminal_at(report_for_pane.into_bytes(), &event.position, client_id)
                .with_context(err_context)?;
            return Ok(MouseEffect::default());
        }

        if let Some(pane) = tab.get_pane_with_id_mut(pane_id) {
            if to_previous_prompt {
                pane.scroll_to_previous_prompt(client_id);
            } else {
                pane.scroll_to_next_prompt(client_id);
            }
        }
        Ok(MouseEffect::state_changed())
    }

    pub(crate) fn handle_scrollwheel_horizontal(
        tab: &mut Tab,
        pane_id: PaneId,
        point: &Position,
        cols: usize,
        scroll_right: bool,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || {
            format!(
                "failed to handle horizontal scrollwheel at position {point:?} for client {client_id}"
            )
        };
        if !matches!(pane_id, PaneId::Plugin(_)) {
            return Ok(MouseEffect::default());
        }
        if let Some(pane) = Self::get_pane_at(tab, point, false).with_context(err_context)? {
            if scroll_right {
                pane.scroll_right(cols, client_id);
            } else {
                pane.scroll_left(cols, client_id);
            }
        }
        Ok(MouseEffect::default())
    }

    fn handle_resize_scroll_up(
        tab: &mut Tab,
        pane_id: PaneId,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || format!("failed to handle resize scroll up for pane {pane_id:?}");

        let is_floating = tab.floating_panes.panes_contain(&pane_id);

        let strategy = ResizeStrategy {
            resize: Resize::Increase,
            direction: None,
            invert_on_boundaries: false,
        };

        if is_floating {
            Self::resize_floating_pane_with_strategies(tab, pane_id, &[strategy], (5, 2))
                .with_context(err_context)?;
            tab.swap_layouts.set_is_floating_damaged();
        } else {
            let active_pane_id = tab
                .get_active_pane_id(client_id)
                .ok_or_else(|| anyhow!("Failed to find active pane"))?;

            tab.dissolve_stack_lists_for_classic_mutation();
            Self::resize_tiled_pane_with_stacked_resize(tab, active_pane_id, &strategy)
                .with_context(err_context)?;
            tab.tiled_panes.reapply_pane_frames();
            tab.swap_layouts.set_is_tiled_damaged();
        }

        tab.set_force_render();
        Ok(MouseEffect::state_changed())
    }

    fn handle_resize_scroll_down(
        tab: &mut Tab,
        pane_id: PaneId,
        client_id: ClientId,
    ) -> Result<MouseEffect> {
        let err_context = || format!("failed to handle resize scroll down for pane {pane_id:?}");

        let is_floating = tab.floating_panes.panes_contain(&pane_id);

        let strategy = ResizeStrategy {
            resize: Resize::Decrease,
            direction: None,
            invert_on_boundaries: false,
        };

        if is_floating {
            Self::resize_floating_pane_with_strategies(tab, pane_id, &[strategy], (5, 2))
                .with_context(err_context)?;
            tab.swap_layouts.set_is_floating_damaged();
        } else {
            let active_pane_id = tab
                .get_active_pane_id(client_id)
                .ok_or_else(|| anyhow!("Failed to find active pane"))?;

            tab.dissolve_stack_lists_for_classic_mutation();
            Self::resize_tiled_pane_with_stacked_resize(tab, active_pane_id, &strategy)
                .with_context(err_context)?;
            tab.tiled_panes.reapply_pane_frames();
            tab.swap_layouts.set_is_tiled_damaged();
        }

        tab.set_force_render();
        Ok(MouseEffect::state_changed())
    }

    fn get_pane_at<'a>(
        tab: &'a mut Tab,
        point: &Position,
        search_selectable: bool,
    ) -> Result<Option<&'a mut Box<dyn Pane>>> {
        let err_context = || format!("failed to get pane at position {point:?}");

        if tab.floating_panes.panes_are_visible() {
            if let Some(pane_id) = tab
                .floating_panes
                .get_pane_id_at(point, search_selectable)
                .with_context(err_context)?
            {
                return Ok(tab.floating_panes.get_pane_mut(pane_id));
            }
        } else if tab.floating_panes.has_pinned_panes() {
            if let Some(pane_id) = tab
                .floating_panes
                .get_pinned_pane_id_at(point, search_selectable)
                .with_context(err_context)?
            {
                return Ok(tab.floating_panes.get_pane_mut(pane_id));
            }
        }
        if let Some(pane_id) = tab
            .get_pane_id_at(point, search_selectable)
            .with_context(err_context)?
        {
            Ok(tab.get_pane_with_id_mut(pane_id))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn set_mouse_selection_support(
        tab: &mut Tab,
        pane_id: PaneId,
        selection_support: bool,
    ) {
        if let Some(pane) = tab.get_pane_with_id_mut(pane_id) {
            pane.set_mouse_selection_support(selection_support);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_event_context(mouse_scroll_resize: bool) -> MouseEventContext {
        MouseEventContext {
            pane_id_at_position: Some(PaneId::Terminal(1)),
            active_pane_id: Some(PaneId::Terminal(1)),
            floating_visible: false,
            input_mode: InputMode::Normal,
            pane_being_resized: false,
            selecting_with_mouse: false,
            pane_being_moved: false,
            acme_handle_drag: None,
            acme_context_menu: None,
            clicked_pane: None,
            advanced_mouse_actions: true,
            acme_vertical_border_hit: false,
            acme_title_pane_id: None,
            acme_title_button_pane_id: None,
            acme_wrap_indicator_pane_id: None,
            pinned_selectable: None,
            pinned_unselectable: None,
            focus_follows_mouse: false,
            mouse_click_through: false,
            mouse_scroll_resize,
            passthrough_pane_id: None,
        }
    }

    #[test]
    fn ctrl_left_click_terminal_content_plumbs_text() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let mut event = MouseEvent::new_left_press_event(position);
        event.ctrl = true;
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: false,
        });

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::PlumbText {
                pane_id: PaneId::Terminal(1),
                position,
            }
        );
    }

    #[test]
    fn middle_click_active_pane_content_pastes_when_terminal_does_not_want_mouse() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: false,
        });

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_middle_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::PasteFromHostClipboard {
                pane_id: PaneId::Terminal(1),
            }
        );
    }

    #[test]
    fn middle_click_inactive_pane_content_pastes_when_terminal_does_not_want_mouse() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.active_pane_id = Some(PaneId::Terminal(2));
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: false,
        });

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_middle_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::PasteFromHostClipboard {
                pane_id: PaneId::Terminal(1),
            }
        );
    }

    #[test]
    fn middle_click_active_pane_content_forwards_when_terminal_wants_mouse() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let event = MouseEvent::new_middle_press_event(position);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: true,
        });

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::SendToTerminal {
                pane_id: PaneId::Terminal(1),
                event,
            }
        );
    }

    #[test]
    fn ctrl_left_click_acme_title_button_creates_pane() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_left_press_with_ctrl_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::NewAcmePane {
                pane_id: PaneId::Terminal(1),
            }
        );
    }

    #[test]
    fn ctrl_right_click_acme_title_button_creates_column() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let mut event = MouseEvent::new_right_press_event(position);
        event.ctrl = true;
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::NewAcmeColumn {
                pane_id: PaneId::Terminal(1),
            }
        );
    }

    #[test]
    fn alt_left_click_acme_title_button_swaps_column_left() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_left_press_with_alt_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::SwapAcmeColumn {
                pane_id: PaneId::Terminal(1),
                direction: Direction::Left,
            }
        );
    }

    #[test]
    fn alt_right_click_acme_title_button_swaps_column_right() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_right_press_with_alt_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::SwapAcmeColumn {
                pane_id: PaneId::Terminal(1),
                direction: Direction::Right,
            }
        );
    }

    #[test]
    fn alt_right_release_acme_title_button_is_ignored() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let mut event = MouseEvent::new_right_release_event(position);
        event.alt = true;
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::NoAction
        );
    }

    #[test]
    fn context_menu_send_bytes_end_with_enter() {
        assert_eq!(text_with_trailing_enter("echo hi".to_owned()), b"echo hi\r");
        assert_eq!(
            text_with_trailing_enter("echo hi\n".to_owned()),
            b"echo hi\n"
        );
        assert_eq!(
            text_with_trailing_enter("echo hi\r".to_owned()),
            b"echo hi\r"
        );
    }

    #[test]
    fn search_mode_mouse_buttons_navigate_matches_on_terminal_content() {
        let mut context = mouse_event_context(false);
        context.input_mode = InputMode::Search;
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: false,
        });
        let position = Position::new(1, 1);

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_left_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::SearchDown
        );
        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_right_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::SearchUp
        );
        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_middle_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::CancelSearch
        );
    }

    #[test]
    fn plain_left_click_on_acme_wrap_indicator_toggles_wrap() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: true,
            frame_intercepted: false,
            edge: None,
            is_acme_title: true,
            is_floating: false,
            terminal_wants_mouse: false,
        });
        context.acme_wrap_indicator_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_left_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::TogglePaneWrap {
                pane_id: PaneId::Terminal(1),
            }
        );
    }

    #[test]
    fn plain_right_click_on_acme_wrap_indicator_is_ignored() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: true,
            frame_intercepted: false,
            edge: None,
            is_acme_title: true,
            is_floating: false,
            terminal_wants_mouse: false,
        });
        context.acme_wrap_indicator_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_right_press_event(position),
                &context,
            )
            .unwrap(),
            MouseAction::NoAction
        );
    }

    #[test]
    fn plain_right_click_on_pane_decoration_does_not_open_context_menu() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let event = MouseEvent::new_right_press_event(position);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: true,
            frame_intercepted: false,
            edge: None,
            is_acme_title: true,
            is_floating: false,
            terminal_wants_mouse: false,
        });
        context.acme_title_button_pane_id = Some(PaneId::Terminal(1));

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::NoAction
        );
    }

    #[test]
    fn right_click_active_mouse_pane_opens_context_menu() {
        let mut context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let event = MouseEvent::new_right_press_event(position);
        context.clicked_pane = Some(ClickedPaneDetails {
            pane_id: PaneId::Terminal(1),
            on_frame: false,
            frame_intercepted: false,
            edge: None,
            is_acme_title: false,
            is_floating: false,
            terminal_wants_mouse: true,
        });

        assert_eq!(
            MouseHandler::determine_mouse_action(&event, &context).unwrap(),
            MouseAction::StartAcmeContextMenu {
                pane_id: PaneId::Terminal(1),
                position,
            }
        );
    }

    #[test]
    fn context_menu_updates_selection_without_moving_inside_box() {
        let mut menu = AcmeContextMenuState::new(
            PaneId::Terminal(1),
            Position::new(1, 1),
            Size { cols: 80, rows: 24 },
        );
        let original_position = (menu.x, menu.y);

        menu.update_for_mouse_position(Position::new(3, 3), Size { cols: 80, rows: 24 });
        assert_eq!((menu.x, menu.y), original_position);
        assert_eq!(menu.selected_action, Some(AcmeContextMenuAction::Send));

        menu.update_for_mouse_position(Position::new(4, 3), Size { cols: 80, rows: 24 });
        assert_eq!((menu.x, menu.y), original_position);
        assert_eq!(menu.selected_action, Some(AcmeContextMenuAction::Look));

        menu.update_for_mouse_position(Position::new(5, 3), Size { cols: 80, rows: 24 });
        assert_eq!((menu.x, menu.y), original_position);
        assert_eq!(
            menu.selected_action,
            Some(AcmeContextMenuAction::GoToDefinition)
        );
        assert_eq!(menu.drag_offset, None);

        menu.update_for_mouse_position(Position::new(6, 3), Size { cols: 80, rows: 24 });
        assert_eq!((menu.x, menu.y), original_position);
        assert_eq!(menu.selected_action, Some(AcmeContextMenuAction::Cancel));
        assert_eq!(menu.drag_offset, None);
    }

    #[test]
    fn context_menu_definition_does_not_drag_menu_or_persist_selection() {
        let mut menu = AcmeContextMenuState::new(
            PaneId::Terminal(1),
            Position::new(1, 1),
            Size { cols: 80, rows: 24 },
        );
        let original_position = (menu.x, menu.y);

        menu.update_for_mouse_position(Position::new(5, 3), Size { cols: 80, rows: 24 });
        assert_eq!(
            menu.selected_action,
            Some(AcmeContextMenuAction::GoToDefinition)
        );
        assert_eq!(menu.drag_offset, None);

        menu.update_for_mouse_position(Position::new(10, 20), Size { cols: 80, rows: 24 });
        assert_eq!((menu.x, menu.y), original_position);
        assert_eq!(menu.selected_action, None);
        assert_eq!(menu.action_for_release(Position::new(10, 20)), None);
    }

    #[test]
    fn context_menu_moves_after_selected_item_leaves_box() {
        let mut menu = AcmeContextMenuState::new(
            PaneId::Terminal(1),
            Position::new(1, 1),
            Size { cols: 80, rows: 24 },
        );
        menu.update_for_mouse_position(Position::new(3, 3), Size { cols: 80, rows: 24 });

        menu.update_for_mouse_position(Position::new(10, 20), Size { cols: 80, rows: 24 });

        assert_eq!((menu.x, menu.y), (19, 8));
        assert_eq!(menu.selected_action, Some(AcmeContextMenuAction::Send));
    }

    #[test]
    fn active_context_menu_tracks_motion_and_finishes_on_release() {
        let mut context = mouse_event_context(false);
        let anchor = Position::new(1, 1);
        context.acme_context_menu = Some(AcmeContextMenuState::new(
            PaneId::Terminal(1),
            anchor,
            Size { cols: 80, rows: 24 },
        ));
        let release_position = Position::new(1, 2);

        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_right_motion_event(release_position),
                &context,
            )
            .unwrap(),
            MouseAction::UpdateAcmeContextMenu {
                position: release_position,
            }
        );
        assert_eq!(
            MouseHandler::determine_mouse_action(
                &MouseEvent::new_right_release_event(release_position),
                &context,
            )
            .unwrap(),
            MouseAction::FinishAcmeContextMenu {
                position: release_position,
            }
        );
    }

    #[test]
    fn disabled_ctrl_scroll_does_not_fall_through_to_regular_scrolling() {
        let context = mouse_event_context(false);
        let position = Position::new(1, 1);
        let events = [
            MouseEvent::new_ctrl_scroll_up_event(position),
            MouseEvent::new_ctrl_scroll_down_event(position),
        ];

        for event in events {
            assert_eq!(
                MouseHandler::determine_mouse_action(&event, &context).unwrap(),
                MouseAction::NoAction
            );
        }
    }
}
