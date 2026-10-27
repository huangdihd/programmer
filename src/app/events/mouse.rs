// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Mouse handling: routing scrolls and clicks between the conversation
//! panel and the sidebar, plus text selection.

use super::super::{App, PointerTarget, session};
use crate::ui::components::conversation_panel::conversation_panel::SelectionEnd;
use crate::ui::components::footer::footer::FooterTarget;
use crate::ui::components::sidebar::ClickTarget;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn contains(area: ratatui::layout::Rect, mouse: MouseEvent) -> bool {
    mouse.column >= area.x
        && mouse.column < area.right()
        && mouse.row >= area.y
        && mouse.row < area.bottom()
}

fn sidebar_pointer_target(app: &App<'_>, mouse: MouseEvent) -> Option<PointerTarget> {
    let sidebar = app.ui.sidebar.as_ref()?;
    let area = sidebar.area()?;
    if !contains(area, mouse) {
        return None;
    }
    if mouse.column == area.x {
        return Some(PointerTarget::SidebarResize);
    }
    let row = mouse.row - area.y;
    sidebar
        .click_map
        .get(usize::from(row))
        .is_some_and(|target| !matches!(target, ClickTarget::None))
        .then_some(PointerTarget::Sidebar { row })
}

fn footer_pointer_target(app: &App<'_>, mouse: MouseEvent) -> Option<PointerTarget> {
    let area = app.ui.footer_area?;
    app.ui
        .footer
        .target_at(area, mouse.column, mouse.row)
        .map(PointerTarget::Footer)
}

fn sidebar_width_for_column(sidebar_right: u16, column: u16) -> u16 {
    let max = sidebar_right.saturating_sub(20).max(1);
    let min = 20.min(max);
    sidebar_right.saturating_sub(column).clamp(min, max)
}

fn update_sidebar_resize(app: &mut App<'_>, column: u16) {
    if let Some(area) = app.ui.sidebar.as_ref().and_then(|sidebar| sidebar.area()) {
        let width = sidebar_width_for_column(area.right(), column);
        app.ui.sidebar_width = width;
        app.ui.sidebar_resize_preview = Some(width);
    }
}

fn commit_sidebar_resize(app: &mut App<'_>, column: u16) {
    update_sidebar_resize(app, column);
    app.ui.sidebar_resize_preview = None;
}

fn pointer_is_on_sidebar_divider(app: &App<'_>, mouse: MouseEvent) -> bool {
    app.ui
        .sidebar
        .as_ref()
        .and_then(|sidebar| sidebar.area())
        .is_some_and(|area| {
            mouse.row >= area.y
                && mouse.row < area.bottom()
                && area.right().saturating_sub(app.ui.sidebar_width) == mouse.column
        })
}

fn activate_footer(app: &mut App<'_>, target: FooterTarget) {
    match target {
        FooterTarget::WorkMode => {
            app.agent_loop.session.work_mode =
                app.agent_loop.session.work_mode.next(app.config.allow_yolo);
            super::super::session::persist_config(app);
        }
        FooterTarget::Security => {
            app.ui.security_panel =
                Some(crate::ui::components::security_panel::SecurityPanel::new());
        }
        FooterTarget::Diagnostics => {
            match crate::ui::components::diagnostics_panel::DiagnosticsPanel::load() {
                Ok(panel) => app.ui.diagnostics_panel = Some(panel),
                Err(error) => {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(format!("could not open diagnostics profile: {error}"));
                    app.ui.conversation_panel.scroll_to_bottom();
                }
            }
        }
        FooterTarget::Model => {
            app.ui.input_panel.set_content("/model ");
            super::update_completions(app);
        }
        FooterTarget::Thinking => {
            app.agent_loop.session.thinking_level = app.agent_loop.session.thinking_level.next();
            app.ui.footer.thinking_level = app.agent_loop.session.thinking_level;
            session::mark_dirty(app);
        }
    }
}

fn option_at(
    area: ratatui::layout::Rect,
    mouse: MouseEvent,
    first_row_offset: u16,
    option_count: usize,
) -> Option<usize> {
    if !contains(area, mouse) {
        return None;
    }
    let first_row = area.y.checked_add(first_row_offset)?;
    if mouse.row < first_row {
        return None;
    }
    let index = usize::from(mouse.row - first_row);
    (index < option_count).then_some(index)
}

/// Route pointer input to the active full-screen management panel.
///
/// Each component owns its hit testing and press/release state; this layer only
/// applies the resulting action to application state, matching keyboard routing.
pub(crate) async fn handle_full_screen_panel_mouse(
    app: &mut App<'_>,
    mouse: MouseEvent,
) -> color_eyre::Result<()> {
    let area = app.ui.frame_area;

    if app.ui.provider_panel.is_some() {
        let action = app
            .ui
            .provider_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, &mut app.config, &app.provider_manager);
        use crate::ui::components::provider_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.provider_panel = None,
            PanelAction::Saved => {
                session::persist_config(app);
                app.events
                    .send(crate::ui::event::AppEvent::ProvidersChanged);
            }
            PanelAction::RefreshModels => {
                app.events
                    .send(crate::ui::event::AppEvent::RefreshProviderModels {
                        name: None,
                        notify: true,
                    });
            }
            PanelAction::None => {}
        }
        return Ok(());
    }

    if app.ui.rewind_panel.is_some() {
        let action = app
            .ui
            .rewind_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, area);
        use crate::ui::components::rewind_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.rewind_panel = None,
            PanelAction::Fork { checkpoint_id } => {
                app.ui.rewind_panel = None;
                super::keys::apply_rewind_fork(app, checkpoint_id).await;
            }
            PanelAction::Restore {
                checkpoint_id,
                mode,
            } => {
                app.ui.rewind_panel = None;
                super::keys::apply_rewind(app, checkpoint_id, mode);
            }
            PanelAction::None => {}
        }
        return Ok(());
    }

    if app.ui.skills_panel.is_some() {
        let action = app
            .ui
            .skills_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, &mut app.agent_loop.skill_registry);
        use crate::ui::components::skills_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.skills_panel = None,
            PanelAction::Saved => session::save_session(app),
            PanelAction::None => {}
        }
        return Ok(());
    }

    if app.ui.mcp_panel.is_some() {
        let action = app
            .ui
            .mcp_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, &mut app.config);
        use crate::ui::components::mcp_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.mcp_panel = None,
            PanelAction::Saved => {
                session::persist_config(app);
                app.events.send(crate::ui::event::AppEvent::McpChanged);
            }
            PanelAction::None => {}
        }
        return Ok(());
    }

    if app.ui.diagnostics_panel.is_some() {
        let action = app
            .ui
            .diagnostics_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, area);
        use crate::ui::components::diagnostics_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.diagnostics_panel = None,
            PanelAction::Saved(profile) => match crate::app::diagnostics::save_profile(&profile) {
                Ok(()) => {
                    crate::app::diagnostics::reset_diagnostics_state(app);
                    app.agent_loop
                        .session
                        .diagnostics_state
                        .lock()
                        .unwrap()
                        .lsp_configured = crate::app::helpers::lsp_checker_configured();
                    crate::app::diagnostics::start_update(app, false);
                }
                Err(error) => {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(format!("could not save diagnostics profile: {error}"));
                    app.ui.conversation_panel.scroll_to_bottom();
                }
            },
            PanelAction::None => {}
        }
        return Ok(());
    }

    if app.ui.security_panel.is_some() {
        let previous_profile = app.config.active_security_profile.clone();
        let previous_security = app.config.security.clone();
        let action = app
            .ui
            .security_panel
            .as_mut()
            .expect("checked above")
            .handle_mouse(mouse, area, &mut app.config);
        use crate::ui::components::security_panel::PanelAction;
        match action {
            PanelAction::Close => app.ui.security_panel = None,
            PanelAction::Saved => session::persist_config(app),
            PanelAction::Apply => match app.install_active_security() {
                Ok(()) => session::persist_config(app),
                Err(error) => {
                    app.config.active_security_profile = previous_profile.clone();
                    app.config
                        .security_profiles
                        .insert(previous_profile, previous_security.clone());
                    app.config.security = previous_security;
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(format!("invalid security configuration: {error}"));
                    app.ui.conversation_panel.scroll_to_bottom();
                }
            },
            PanelAction::None => {}
        }
    }

    Ok(())
}

/// Route a mouse event to the active overlay, sidebar, or conversation panel.
pub(crate) async fn handle_mouse(app: &mut App<'_>, mouse: MouseEvent) -> color_eyre::Result<()> {
    if let Some(viewer) = app.ui.image_viewer.as_mut() {
        match mouse.kind {
            MouseEventKind::ScrollUp => viewer.zoom_in(),
            MouseEventKind::ScrollDown => viewer.zoom_out(),
            MouseEventKind::Down(MouseButton::Left) => viewer.begin_drag(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) => viewer.drag_to(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => viewer.end_drag(),
            _ => {}
        }
        return Ok(());
    }

    let completion_visible = app
        .ui
        .input_panel
        .completion
        .as_ref()
        .is_some_and(|completion| completion.visible);
    let overlay_owns_pointer = app.ui.todo_panel.is_some()
        || app.ui.question_panel.is_some()
        || app.agent_loop.pending_review.is_some()
        || (app.agent_loop.session.work_mode == crate::classifier::WorkMode::Plan
            && app.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing)
        || completion_visible;
    if overlay_owns_pointer {
        // Overlays leave the conversation visible behind them, so discard its
        // independent pointer highlight before routing feedback to the top layer.
        app.ui.conversation_panel.clear_pointer_feedback();
    }

    if app.ui.todo_panel.is_some() {
        let hovered = app.ui.todo_area.and_then(|area| {
            let panel = app.ui.todo_panel.as_ref()?;
            panel
                .mouse_index(area, mouse)
                .map(|index| PointerTarget::Todo { index })
        });
        app.ui.set_pointer_hover(hovered.clone());
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                app.ui.set_pointer_pressed(hovered);
                return Ok(());
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let activate = hovered.is_some() && app.ui.take_pointer_pressed() == hovered;
                if !activate {
                    return Ok(());
                }
            }
            _ => {}
        }
        let mouse_key = app.ui.todo_area.and_then(|area| {
            app.ui
                .todo_panel
                .as_mut()
                .and_then(|panel| panel.mouse_key(area, mouse))
        });
        if let Some(code) = mouse_key {
            super::keys::handle_key_events(app, key(code)).await?;
        }
        return Ok(());
    }

    if app.ui.question_panel.is_some() {
        let action = app
            .ui
            .bottom_interaction_area
            .and_then(|area| {
                app.ui
                    .question_panel
                    .as_mut()
                    .map(|panel| panel.handle_mouse(area, mouse))
            })
            .unwrap_or(crate::ui::components::question_panel::AnswerAction::None);
        match action {
            crate::ui::components::question_panel::AnswerAction::Answer(text) => {
                if let Some(panel) = app.ui.question_panel.as_mut() {
                    panel.answer(text);
                }
                app.ui.question_panel = None;
                app.ui.clear_pointer_feedback();
                return Ok(());
            }
            crate::ui::components::question_panel::AnswerAction::SelectModel(model) => {
                if let Err(error) =
                    super::super::command_handlers::settings::switch_model(app, &model)
                {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(error);
                    app.ui.conversation_panel.scroll_to_bottom();
                }
                super::super::peers::refresh_consent_models(app);
                return Ok(());
            }
            crate::ui::components::question_panel::AnswerAction::None => {}
        }
        let hovered = app.ui.bottom_interaction_area.and_then(|area| {
            app.ui
                .question_panel
                .as_ref()
                .and_then(|panel| panel.mouse_option(area, mouse))
                .map(|index| PointerTarget::Question { index })
        });
        app.ui.set_pointer_hover(hovered.clone());
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.ui.set_pointer_pressed(hovered);
            return Ok(());
        }
        let activate_press = if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            let pressed = app.ui.take_pointer_pressed();
            hovered.is_some() && pressed == hovered
        } else {
            true
        };
        let activate = activate_press
            && app.ui.bottom_interaction_area.is_some_and(|area| {
                app.ui
                    .question_panel
                    .as_mut()
                    .is_some_and(|panel| panel.select_mouse_option(area, mouse))
            });
        if activate {
            super::keys::handle_key_events(app, key(KeyCode::Enter)).await?;
        }
        return Ok(());
    }

    if app.agent_loop.pending_review.is_some() {
        let hovered = app.ui.bottom_interaction_area.and_then(|area| {
            let detail_count = app
                .agent_loop
                .pending_review
                .as_ref()
                .map(|review| {
                    crate::ui::tool_details::format_tool_details(
                        &review.call.name,
                        &review.call.arguments,
                    )
                    .len() as u16
                })
                .unwrap_or_default();
            option_at(area, mouse, 3u16.saturating_add(detail_count), 2)
                .map(|index| PointerTarget::Approval { index })
        });
        app.ui.set_pointer_hover(hovered.clone());
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.ui.set_pointer_pressed(hovered);
            return Ok(());
        }
        let activate = if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            let pressed = app.ui.take_pointer_pressed();
            hovered.is_some() && pressed == hovered
        } else {
            false
        };
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => Some(KeyCode::Up),
            MouseEventKind::ScrollDown => Some(KeyCode::Down),
            MouseEventKind::Up(MouseButton::Left) if activate => {
                app.ui.bottom_interaction_area.and_then(|area| {
                    let detail_count = app
                        .agent_loop
                        .pending_review
                        .as_ref()
                        .map(|review| {
                            crate::ui::tool_details::format_tool_details(
                                &review.call.name,
                                &review.call.arguments,
                            )
                            .len() as u16
                        })
                        .unwrap_or_default();
                    let selected = option_at(area, mouse, 3u16.saturating_add(detail_count), 2)?;
                    app.agent_loop.pending_review.as_mut().unwrap().selected = selected;
                    Some(KeyCode::Enter)
                })
            }
            _ => None,
        };
        if let Some(code) = code {
            super::keys::handle_key_events(app, key(code)).await?;
        }
        return Ok(());
    }

    if app.agent_loop.session.work_mode == crate::classifier::WorkMode::Plan
        && app.agent_loop.plan_phase == crate::classifier::PlanPhase::Reviewing
    {
        let option_count = if app.config.allow_yolo { 4 } else { 3 };
        let hovered = app
            .ui
            .bottom_interaction_area
            .and_then(|area| option_at(area, mouse, 3, option_count))
            .map(|index| PointerTarget::Plan { index });
        app.ui.set_pointer_hover(hovered.clone());
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.ui.set_pointer_pressed(hovered);
            return Ok(());
        }
        let activate = if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            let pressed = app.ui.take_pointer_pressed();
            hovered.is_some() && pressed == hovered
        } else {
            false
        };
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => Some(KeyCode::Up),
            MouseEventKind::ScrollDown => Some(KeyCode::Down),
            MouseEventKind::Up(MouseButton::Left) if activate => {
                app.ui.bottom_interaction_area.and_then(|area| {
                    let selected = option_at(area, mouse, 3, option_count)?;
                    app.ui.plan_review_selected = selected;
                    Some(KeyCode::Enter)
                })
            }
            _ => None,
        };
        if let Some(code) = code {
            super::keys::handle_key_events(app, key(code)).await?;
        }
        return Ok(());
    }

    let completion_owns_pointer = app
        .ui
        .input_panel
        .completion
        .as_ref()
        .is_some_and(|completion| completion.visible)
        && (app
            .ui
            .completion_area
            .is_some_and(|area| contains(area, mouse))
            || matches!(
                app.ui.pointer_pressed,
                Some(PointerTarget::Completion { .. })
            ));
    if completion_owns_pointer {
        let hovered = app.ui.completion_area.and_then(|area| {
            if !contains(area, mouse) {
                return None;
            }
            let completion = app.ui.input_panel.completion.as_ref()?;
            let index = completion.scroll_offset + usize::from(mouse.row - area.y);
            (index < completion.candidates.len()).then_some(PointerTarget::Completion { index })
        });
        app.ui.set_pointer_hover(hovered.clone());
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.ui.set_pointer_pressed(hovered);
            return Ok(());
        }
        let activate = if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            let pressed = app.ui.take_pointer_pressed();
            hovered.is_some() && pressed == hovered
        } else {
            false
        };
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => Some(KeyCode::Up),
            MouseEventKind::ScrollDown => Some(KeyCode::Down),
            MouseEventKind::Up(MouseButton::Left) if activate => {
                let selected = app.ui.completion_area.and_then(|area| {
                    if !contains(area, mouse) {
                        return None;
                    }
                    let completion = app.ui.input_panel.completion.as_ref()?;
                    let selected = completion.scroll_offset + usize::from(mouse.row - area.y);
                    (selected < completion.candidates.len()).then_some(selected)
                });
                if let Some(selected) = selected {
                    app.ui.input_panel.accept_completion(selected);
                }
                None
            }
            _ => None,
        };
        if let Some(code) = code {
            super::keys::handle_key_events(app, key(code)).await?;
        }
        return Ok(());
    }

    let hovered = sidebar_pointer_target(app, mouse).or_else(|| footer_pointer_target(app, mouse));
    app.ui.set_pointer_hover(hovered);
    match mouse.kind {
        MouseEventKind::Moved => app
            .ui
            .conversation_panel
            .pointer_moved(mouse.column, mouse.row),
        MouseEventKind::ScrollDown => {
            if app
                .ui
                .sidebar
                .as_ref()
                .and_then(|sidebar| sidebar.area())
                .is_some_and(|area| contains(area, mouse))
            {
                if let Some(ref mut s) = app.ui.sidebar {
                    s.scroll_by(3);
                }
            } else {
                app.ui.conversation_panel.scroll_down();
            }
        }
        MouseEventKind::ScrollUp => {
            if app
                .ui
                .sidebar
                .as_ref()
                .and_then(|sidebar| sidebar.area())
                .is_some_and(|area| contains(area, mouse))
            {
                if let Some(ref mut s) = app.ui.sidebar {
                    s.scroll_up_by(3);
                }
            } else {
                app.ui.conversation_panel.scroll_up();
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.ui.conversation_click_consumed = false;
            app.ui.set_pointer_pressed(app.ui.pointer_hover.clone());
            if matches!(app.ui.pointer_pressed, Some(PointerTarget::SidebarResize)) {
                app.ui.sidebar_resizing = true;
                app.ui.sidebar_resize_main = None;
                update_sidebar_resize(app, mouse.column);
                return Ok(());
            }
            if matches!(app.ui.pointer_pressed, Some(PointerTarget::Footer(_))) {
                return Ok(());
            }
            // Capture the exact sidebar target now so an async redraw cannot
            // make mouse-up activate a different row.
            app.ui.sidebar_click_target = app.ui.sidebar.as_ref().and_then(|sidebar| {
                let area = sidebar.area()?;
                if !contains(area, mouse) {
                    return None;
                }
                let row = mouse.row - area.y;
                sidebar
                    .click_map
                    .get(usize::from(row))
                    .cloned()
                    .map(|target| (target, row))
            });
            if app.ui.sidebar_click_target.is_some() {
                return Ok(());
            }
            app.ui
                .conversation_panel
                .pointer_down(mouse.column, mouse.row);
            // The scrollbar track is always hit-testable, even while its thumb
            // is auto-hidden. A track click jumps there and starts a drag.
            if app
                .ui
                .conversation_panel
                .begin_scrollbar_drag(mouse.column, mouse.row)
            {
                app.ui.conversation_click_consumed = true;
                return Ok(());
            }
            // The jump button follows the same press/release lifecycle as the
            // other controls, so dragging away cancels activation.
            if app
                .ui
                .conversation_panel
                .jump_button_hit(mouse.column, mouse.row)
            {
                app.ui.conversation_click_consumed = true;
                return Ok(());
            }
            app.ui
                .conversation_panel
                .selection_begin(mouse.column, mouse.row)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.ui.conversation_panel.scrollbar_dragging() {
                app.ui.conversation_panel.drag_scrollbar(mouse.row);
                return Ok(());
            }
            if app.ui.sidebar_resizing {
                update_sidebar_resize(app, mouse.column);
                app.ui.set_pointer_hover(Some(PointerTarget::SidebarResize));
                return Ok(());
            }
            if app.ui.sidebar_click_target.is_some() || app.ui.conversation_click_consumed {
                return Ok(());
            }
            app.ui
                .conversation_panel
                .pointer_dragged(mouse.column, mouse.row);
            app.ui
                .conversation_panel
                .selection_drag(mouse.column, mouse.row)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.ui.conversation_panel.end_scrollbar_drag() {
                app.ui
                    .conversation_panel
                    .pointer_up(mouse.column, mouse.row);
                app.ui.conversation_click_consumed = false;
                return Ok(());
            }
            if app.ui.sidebar_resizing {
                commit_sidebar_resize(app, mouse.column);
                app.ui.sidebar_resizing = false;
                app.ui.sidebar_resize_main = None;
                app.ui.set_pointer_pressed(None);
                app.ui.set_pointer_hover(
                    pointer_is_on_sidebar_divider(app, mouse)
                        .then_some(PointerTarget::SidebarResize),
                );
                return Ok(());
            }
            let released_pointer = app.ui.pointer_hover.clone();
            let pressed_pointer = app.ui.take_pointer_pressed();
            if let (Some(PointerTarget::Footer(pressed)), Some(PointerTarget::Footer(released))) =
                (&pressed_pointer, &released_pointer)
            {
                if pressed == released {
                    activate_footer(app, *released);
                }
                return Ok(());
            }
            if let Some((target, pressed_row)) = app.ui.sidebar_click_target.take() {
                if pressed_pointer != released_pointer {
                    return Ok(());
                }
                let same_row = app
                    .ui
                    .sidebar
                    .as_ref()
                    .and_then(|sidebar| sidebar.area())
                    .is_some_and(|area| contains(area, mouse) && mouse.row - area.y == pressed_row);
                if same_row {
                    handle_sidebar_click(app, &target);
                }
                return Ok(());
            }
            if app.ui.conversation_click_consumed {
                let jump = app
                    .ui
                    .conversation_panel
                    .jump_button_activates_at(mouse.column, mouse.row);
                app.ui
                    .conversation_panel
                    .pointer_up(mouse.column, mouse.row);
                app.ui.conversation_click_consumed = false;
                if jump {
                    app.ui.conversation_panel.scroll_to_bottom();
                }
                return Ok(());
            }
            let selection_end = app
                .ui
                .conversation_panel
                .selection_end(mouse.column, mouse.row);
            app.ui
                .conversation_panel
                .pointer_up(mouse.column, mouse.row);
            match selection_end {
                SelectionEnd::Click { column, row } => {
                    if let Err(error) = app.ui.conversation_panel.handle_buffer_click(column, row) {
                        app.agent_loop
                            .session
                            .conversation
                            .lock()
                            .unwrap()
                            .add_error_string(error);
                        app.ui.conversation_panel.scroll_to_bottom();
                    }
                }
                SelectionEnd::Link(url) => {
                    if let Err(error) = crate::ui::components::conversation_panel::links::open(&url)
                    {
                        app.agent_loop
                            .session
                            .conversation
                            .lock()
                            .unwrap()
                            .add_error_string(format!("failed to open link: {error}"));
                        app.ui.conversation_panel.scroll_to_bottom();
                    }
                }
                SelectionEnd::Copied(text) => {
                    if crate::clipboard::copy(&text) {
                        app.ui.conversation_panel.show_copied();
                    } else {
                        app.agent_loop
                            .session
                            .conversation
                            .lock()
                            .unwrap()
                            .add_error_string("failed to copy selection to clipboard");
                        app.ui.conversation_panel.scroll_to_bottom();
                        session::save_session(app);
                    }
                }
                SelectionEnd::Image(image_id) => {
                    app.ui.image_viewer = app.ui.conversation_panel.image_viewer(image_id);
                    app.ui.clear_pointer_feedback();
                    app.ui.conversation_panel.cancel_pointer_gesture();
                }
                SelectionEnd::Ignored => {}
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_sidebar_click(app: &mut App<'_>, target: &ClickTarget) {
    match target {
        ClickTarget::Section(key) => {
            if let Some(ref mut s) = app.ui.sidebar {
                s.toggle_section(*key);
            }
        }
        ClickTarget::TodoItem(idx) => {
            let mut sorted: Vec<&crate::todos::Todo> = app.ui.todo_list.todos.iter().collect();
            sorted
                .sort_by_key(|t| crate::ui::components::sidebar::ui::todo_status_order(&t.status));
            if let Some(todo) = sorted.get(*idx) {
                let id = todo.id.clone();
                let result = app
                    .agent_loop
                    .session
                    .todo_store
                    .lock()
                    .unwrap()
                    .toggle_status(&id)
                    .map(|_| ());
                if let Err(error) = result {
                    app.agent_loop
                        .session
                        .conversation
                        .lock()
                        .unwrap()
                        .add_error_string(error);
                    app.ui.conversation_panel.scroll_to_bottom();
                }
                app.sync_todos_from_store();
                session::mark_dirty(app);
            }
        }
        ClickTarget::Task(id) => {
            if let Some(ref mut s) = app.ui.sidebar {
                s.toggle_task(*id);
            }
        }
        ClickTarget::Agent(id) => {
            if let (Some(snapshot), Some(conversation)) = (
                app.agent_loop.session.agents.snapshot(*id),
                app.agent_loop.session.agents.conversation(*id),
            ) {
                app.ui.agent_panel = Some(crate::ui::components::agent_panel::AgentPanel::new(
                    app.agent_loop.session.tasks.clone(),
                    *id,
                    snapshot.name,
                    conversation,
                ));
            }
        }
        ClickTarget::Diagnostic(_idx) => {
            // Could jump to file location in the future.
        }
        ClickTarget::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse_with_kind(column: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn mouse(column: u16, row: u16) -> MouseEvent {
        mouse_with_kind(column, row, MouseEventKind::Up(MouseButton::Left))
    }

    #[test]
    fn option_hit_testing_rejects_headers_gaps_and_outside_columns() {
        let area = ratatui::layout::Rect::new(10, 5, 30, 8);

        assert_eq!(option_at(area, mouse(12, 7), 3, 2), None);
        assert_eq!(option_at(area, mouse(12, 8), 3, 2), Some(0));
        assert_eq!(option_at(area, mouse(12, 9), 3, 2), Some(1));
        assert_eq!(option_at(area, mouse(12, 10), 3, 2), None);
        assert_eq!(option_at(area, mouse(9, 8), 3, 2), None);
        assert_eq!(option_at(area, mouse(40, 8), 3, 2), None);
    }

    #[test]
    fn sidebar_drag_width_tracks_the_divider_and_clamps_the_main_pane() {
        assert_eq!(sidebar_width_for_column(120, 88), 32);
        assert_eq!(sidebar_width_for_column(120, 70), 50);
        assert_eq!(sidebar_width_for_column(120, 110), 20);
        assert_eq!(sidebar_width_for_column(120, 0), 100);
        assert_eq!(sidebar_width_for_column(15, 0), 1);
    }

    #[tokio::test]
    async fn sidebar_divider_turns_focus_blue_on_hover() {
        let mut app = crate::app::commands::tests::command_test_app().await;
        let area = ratatui::layout::Rect::new(0, 0, 120, 30);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut app, area, &mut buffer);
        let divider = area.right() - app.ui.sidebar_width;

        handle_mouse(&mut app, mouse_with_kind(divider, 5, MouseEventKind::Moved))
            .await
            .unwrap();
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut app, area, &mut buffer);

        assert_eq!(buffer[(divider, 5)].symbol(), "│");
        assert_eq!(
            buffer[(divider, 5)].fg,
            crate::ui::markdown_theme::palette::BLUE
        );
    }

    #[tokio::test]
    async fn sidebar_drag_reflows_live_at_the_coalesced_frame_rate() {
        let mut app = crate::app::commands::tests::command_test_app().await;
        let area = ratatui::layout::Rect::new(0, 0, 120, 30);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut app, area, &mut buffer);
        let initial_width = app.ui.sidebar_width;
        let divider = area.right() - initial_width;

        handle_mouse(
            &mut app,
            mouse_with_kind(divider, 5, MouseEventKind::Down(MouseButton::Left)),
        )
        .await
        .unwrap();
        handle_mouse(
            &mut app,
            mouse_with_kind(70, 5, MouseEventKind::Drag(MouseButton::Left)),
        )
        .await
        .unwrap();

        assert_ne!(app.ui.sidebar_width, initial_width);
        assert_eq!(app.ui.sidebar_width, 50);
        assert_eq!(app.ui.sidebar_resize_preview, Some(50));
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut app, area, &mut buffer);
        assert_eq!(app.ui.sidebar.as_ref().unwrap().area().unwrap().x, 70);
        assert_eq!(buffer[(70, 1)].symbol(), "┬");
        assert_eq!(buffer[(70, 2)].symbol(), "│");

        handle_mouse(&mut app, mouse(70, 5)).await.unwrap();
        assert_eq!(app.ui.sidebar_width, 50);
        assert_eq!(app.ui.sidebar_resize_preview, None);
        assert!(!app.ui.sidebar_resizing);
    }

    #[tokio::test]
    async fn conversation_scrollbar_drag_is_routed_before_text_selection() {
        let mut app = crate::app::commands::tests::command_test_app().await;
        app.ui
            .conversation_panel
            .scroll_view_state
            .update(1_000, 100);
        app.ui
            .conversation_panel
            .update_scrollbar_geometry(ratatui::layout::Rect::new(79, 5, 1, 20));

        handle_mouse(
            &mut app,
            mouse_with_kind(79, 15, MouseEventKind::Down(MouseButton::Left)),
        )
        .await
        .unwrap();
        assert!(app.ui.conversation_panel.scrollbar_dragging());
        assert!((400..=500).contains(&app.ui.conversation_panel.scroll_view_state.offset().y));

        handle_mouse(
            &mut app,
            mouse_with_kind(79, 24, MouseEventKind::Drag(MouseButton::Left)),
        )
        .await
        .unwrap();
        assert_eq!(app.ui.conversation_panel.scroll_view_state.offset().y, 900);

        handle_mouse(&mut app, mouse(79, 24)).await.unwrap();
        assert!(!app.ui.conversation_panel.scrollbar_dragging());
    }

    #[tokio::test]
    async fn footer_thinking_cycles_independently_and_marks_the_session_dirty() {
        let mut app = crate::app::commands::tests::command_test_app().await;
        let area = ratatui::layout::Rect::new(0, 20, 100, 1);
        app.ui.footer_area = Some(area);
        app.ui.footer.current_model = "provider/model".into();

        let thinking = app
            .ui
            .footer
            .regions(area)
            .into_iter()
            .find(|(target, _)| *target == FooterTarget::Thinking)
            .unwrap()
            .1;
        handle_mouse(
            &mut app,
            mouse_with_kind(
                thinking.x,
                thinking.y,
                MouseEventKind::Down(MouseButton::Left),
            ),
        )
        .await
        .unwrap();
        handle_mouse(&mut app, mouse(thinking.x, thinking.y))
            .await
            .unwrap();

        assert_eq!(
            app.agent_loop.session.thinking_level,
            crate::thinking::ThinkingLevel::None
        );
        assert_eq!(
            app.ui.footer.thinking_level,
            crate::thinking::ThinkingLevel::None
        );
        assert!(app.agent_loop.session.dirty);

        let model = app
            .ui
            .footer
            .regions(area)
            .into_iter()
            .find(|(target, _)| *target == FooterTarget::Model)
            .unwrap()
            .1;
        handle_mouse(
            &mut app,
            mouse_with_kind(model.x, model.y, MouseEventKind::Down(MouseButton::Left)),
        )
        .await
        .unwrap();
        handle_mouse(&mut app, mouse(model.x, model.y))
            .await
            .unwrap();

        assert_eq!(app.ui.input_panel.get_content(), "/model ");
        assert_eq!(
            app.agent_loop.session.thinking_level,
            crate::thinking::ThinkingLevel::None
        );
    }
}
