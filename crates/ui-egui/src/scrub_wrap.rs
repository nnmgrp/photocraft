//! Status-bar and panel scrubbing, with infinite cursor hold and absolute cursor-warp movement removed from the value.

use egui::{Pos2, Rect, Response, Ui, Vec2};

#[derive(Clone, Copy)]
struct Scrub {
    precise: f64,
    anchor: Pos2,
    confined: bool,
    pending: Option<(Pos2, Pos2)>,
}

pub(crate) fn active(ui: &Ui) -> bool {
    !cfg!(target_arch = "wasm32") && ui.input(|i| i.focused && !i.any_touches() && !i.events.iter().any(|e| matches!(e, egui::Event::PointerGone)))
}

fn wrap_target(pos: Pos2, viewport: Rect) -> Option<Pos2> {
    if !viewport.is_finite() || viewport.width() < 32.0 || !pos.is_finite() || pos.y < viewport.top() || pos.y > viewport.bottom() {
        return None;
    }
    if pos.x <= viewport.left() + 1.0 {
        Some(Pos2::new(viewport.right() - 3.0, pos.y))
    } else if pos.x >= viewport.right() - 1.0 {
        Some(Pos2::new(viewport.left() + 3.0, pos.y))
    } else {
        None
    }
}

fn unwarped(delta: Vec2, pos: Pos2, pending: &mut Option<(Pos2, Pos2)>) -> Vec2 {
    if let Some((from, to)) = *pending {
        if from == to {
            *pending = None;
            return delta;
        }
        let step = to - from;
        let middle = from + step * 0.5;
        if (pos - middle).dot(step) > 0.0 {
            *pending = None;
            return delta - step;
        }
    }
    delta
}

pub(crate) fn is_holding(ui: &Ui, resp: &Response) -> bool {
    ui.data(|d| d.get_temp::<Scrub>(resp.id.with("wrapped-scrub"))).is_some_and(|s| s.confined)
}

pub(crate) fn release(ui: &Ui, id: egui::Id) {
    let key = id.with("wrapped-scrub");
    if let Some(scrub) = ui.data(|d| d.get_temp::<Scrub>(key))
        && (scrub.confined || scrub.pending.is_some())
    {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
        if scrub.pending.is_some() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(scrub.anchor));
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|d| d.remove::<Scrub>(key));
}

/// Tracks cursor confinement, anchor hold and warp compensation for an active drag `resp`.
/// Returns `(unwarped_delta, holding)`.
pub(crate) fn drag_wrap(ui: &Ui, resp: &Response) -> (Vec2, bool) {
    let key = resp.id.with("wrapped-scrub");
    let dragging = active(ui) && resp.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.pointer.primary_down());
    if !dragging {
        release(ui, resp.id);
        return (Vec2::ZERO, false);
    }
    let (pos, delta, viewport) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.delta(), i.viewport_rect()));
    let Some(pos) = pos else {
        return (ui.input(|i| i.pointer.delta()), false);
    };
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub {
        precise: 0.0,
        anchor: ui.input(|i| i.pointer.press_origin()).unwrap_or(pos),
        confined: false,
        pending: None,
    });
    if !scrub.confined {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
        scrub.confined = true;
    }
    let delta = unwarped(delta, pos, &mut scrub.pending);
    if scrub.pending.is_none() && ((pos - scrub.anchor).length() > 16.0 || wrap_target(pos, viewport).is_some()) {
        let to = scrub.anchor;
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(to));
        scrub.pending = Some((pos, to));
        ui.ctx().request_repaint();
    }
    ui.data_mut(|d| d.insert_temp(key, scrub));
    (delta, true)
}

pub(crate) fn update(ui: &Ui, resp: &Response, value: &mut f32, before: f32, range: std::ops::RangeInclusive<f32>, speed: f64, wrapping: bool) {
    let key = resp.id.with("wrapped-scrub");
    if !wrapping {
        return;
    }
    let had_scrub = ui.data(|d| d.get_temp::<Scrub>(key)).is_some();
    let (delta, holding) = drag_wrap(ui, resp);
    if holding {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    }
    if !holding {
        return;
    }
    let (slow, aim) = ui.input(|i| (i.modifiers.shift, i.aim_radius()));
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub { precise: f64::from(before), anchor: Pos2::ZERO, confined: true, pending: None });
    if !had_scrub || !scrub.precise.is_finite() {
        scrub.precise = f64::from(before);
    }
    let speed = if slow { speed / 10.0 } else { speed };
    let change = f64::from(delta.x - delta.y) * speed;
    let (lo, hi) = (f64::from(*range.start()), f64::from(*range.end()));
    if change.is_finite() && change != 0.0 && lo <= hi {
        scrub.precise = (scrub.precise + change).clamp(lo, hi);
        let aim = f64::from(aim) * speed;
        let rounded = egui::emath::smart_aim::best_in_range_f64(scrub.precise - aim, scrub.precise + aim);
        *value = egui::emath::round_to_decimals(rounded, 3).clamp(lo, hi) as f32;
    }
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_numeric_value_step(speed));
    ui.data_mut(|d| d.insert_temp(key, scrub));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn warp_preserves_real_motion_in_the_acknowledgement_frame() {
        for (from, to, motion) in [(pos2(0.5, 600.0), pos2(997.0, 600.0), vec2(-8.0, 2.0)), (pos2(999.5, 600.0), pos2(3.0, 600.0), vec2(8.0, -2.0))] {
            let mut pending = Some((from, to));
            assert_eq!(unwarped(Vec2::ZERO, from, &mut pending), Vec2::ZERO, "wait for an actual warp acknowledgement");
            assert!(pending.is_some());
            assert_eq!(unwarped(to - from + motion, to + motion, &mut pending), motion);
            assert!(pending.is_none());
            assert_eq!(unwarped(motion, to + motion * 2.0, &mut pending), motion, "ordinary movement after the warp");
        }
    }

    /// Returning to an anchor inside the viewport (either left or right of the drag) preserves
    /// the real pointer motion without injecting or dropping a jump.
    #[test]
    fn anchor_return_inside_viewport_preserves_real_motion_in_both_directions() {
        let anchor = pos2(400.0, 300.0);
        // Dragged right of the anchor, then warped back to the anchor.
        let from_right = pos2(435.0, 302.0);
        let motion_right = vec2(3.0, -1.5);
        let mut pending_right = Some((from_right, anchor));
        assert_eq!(unwarped(Vec2::ZERO, from_right, &mut pending_right), Vec2::ZERO);
        assert_eq!(unwarped(anchor - from_right + motion_right, anchor + motion_right, &mut pending_right), motion_right);
        assert!(pending_right.is_none());

        // Dragged left of the anchor, then warped back to the anchor.
        let from_left = pos2(365.0, 298.0);
        let motion_left = vec2(-2.5, 1.0);
        let mut pending_left = Some((from_left, anchor));
        assert_eq!(unwarped(Vec2::ZERO, from_left, &mut pending_left), Vec2::ZERO);
        assert_eq!(unwarped(anchor - from_left + motion_left, anchor + motion_left, &mut pending_left), motion_left);
        assert!(pending_left.is_none());

        // Reversing past the midpoint cancels the warp cleanly.
        let mut reversed = Some((pos2(410.0, 300.0), pos2(430.0, 300.0)));
        let returned = pos2(432.0, 300.0);
        assert_eq!(unwarped(returned - pos2(410.0, 300.0), returned, &mut reversed), returned - pos2(430.0, 300.0));
        assert!(reversed.is_none());
    }

    #[test]
    fn warp_uses_logical_viewport_edges_even_when_motion_crosses_the_edge() {
        let rect = Rect::from_min_size(pos2(20.0, 30.0), vec2(800.0, 600.0));
        assert_eq!(wrap_target(pos2(20.5, 500.0), rect), Some(pos2(817.0, 500.0)));
        assert_eq!(wrap_target(pos2(819.5, 500.0), rect), Some(pos2(23.0, 500.0)));
        assert_eq!(wrap_target(pos2(19.0, 500.0), rect), Some(pos2(817.0, 500.0)));
        assert_eq!(wrap_target(pos2(20.5, 631.0), rect), None);
        assert_eq!(wrap_target(rect.center(), rect), None);
        assert_eq!(wrap_target(pos2(0.5, 10.0), Rect::from_min_size(Pos2::ZERO, vec2(8.0, 20.0))), None);
    }
}
