//! Panel and status-bar scrubbing, with absolute cursor-warp movement removed from the value.
//!
//! A drag holds the cursor in the window and hides it. To keep a scrub going past a viewport edge,
//! the pointer is warped to the opposite edge; the jump the OS makes is taken back out of the
//! frame's delta (see [`unwarped`]). That only works while the jump is far larger than a frame of
//! real motion, so warps happen at the viewport's edges alone. Warping back to the press point
//! every few points, as an earlier version did, makes the jump as small as a fast drag's own
//! per-frame motion: the frame carrying the jump then can't be told from an ordinary one, the jump
//! stays in the delta, and it drags the value towards that end of its range — zero for most
//! fields — where the clamp holds it.

use egui::{Pos2, Rect, Response, Ui, Vec2};

#[derive(Clone, Copy)]
struct Scrub {
    precise: f64,
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

/// [`delta`](Vec2) with the jump of a pending edge warp taken out.
///
/// The warp reaches the app through the OS, so the frame that carries it holds the whole jump in
/// its delta. The pointer coming back past the jump's midpoint is what tells that frame apart from
/// one where the drag merely reversed; an edge warp spans most of the viewport, so no frame of real
/// motion can reach that midpoint in time.
fn unwarped(delta: Vec2, pos: Pos2, pending: &mut Option<(Pos2, Pos2)>) -> Vec2 {
    let Some((from, to)) = *pending else { return delta };
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
    delta
}

pub(crate) fn is_holding(ui: &Ui, resp: &Response) -> bool {
    ui.data(|d| d.get_temp::<Scrub>(resp.id.with("wrapped-scrub"))).is_some_and(|s| s.confined)
}

pub(crate) fn release(ui: &Ui, id: egui::Id) {
    let key = id.with("wrapped-scrub");
    if ui.data(|d| d.get_temp::<Scrub>(key)).is_some_and(|s| s.confined) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
    }
    ui.data_mut(|d| d.remove::<Scrub>(key));
}

/// Tracks cursor confinement and warp compensation for an active drag `resp`.
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
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub { precise: 0.0, confined: false, pending: None });
    if !scrub.confined {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
        scrub.confined = true;
    }
    let delta = unwarped(delta, pos, &mut scrub.pending);
    if scrub.pending.is_none()
        && let Some(to) = wrap_target(pos, viewport)
    {
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
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub { precise: f64::from(before), confined: true, pending: None });
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

    /// A warp in either direction across the viewport keeps the real pointer motion, and reversing
    /// before it lands cancels it cleanly instead of injecting a jump.
    #[test]
    fn edge_warp_in_both_directions_preserves_real_motion() {
        // Warped from the right edge to the left one.
        let from_right = pos2(997.0, 302.0);
        let to_left = pos2(3.0, 302.0);
        let motion_right = vec2(-3.0, -1.5);
        let mut pending_right = Some((from_right, to_left));
        assert_eq!(unwarped(Vec2::ZERO, from_right, &mut pending_right), Vec2::ZERO);
        assert_eq!(unwarped(to_left - from_right + motion_right, to_left + motion_right, &mut pending_right), motion_right);
        assert!(pending_right.is_none());

        // Warped from the left edge to the right one.
        let from_left = pos2(3.0, 298.0);
        let to_right = pos2(997.0, 298.0);
        let motion_left = vec2(2.5, 1.0);
        let mut pending_left = Some((from_left, to_right));
        assert_eq!(unwarped(Vec2::ZERO, from_left, &mut pending_left), Vec2::ZERO);
        assert_eq!(unwarped(to_right - from_left + motion_left, to_right + motion_left, &mut pending_left), motion_left);
        assert!(pending_left.is_none());

        // Reversing past the midpoint before the warp lands cancels it.
        let mut reversed = Some((pos2(990.0, 300.0), pos2(10.0, 300.0)));
        let returned = pos2(480.0, 300.0);
        assert_eq!(unwarped(returned - pos2(990.0, 300.0), returned, &mut reversed), returned - pos2(10.0, 300.0));
        assert!(reversed.is_none());
    }

    /// A jump no larger than a frame of real motion can't be told from one, so nothing but the
    /// viewport's edges may warp: a small pin-to-press-point warp would leave its own jump in the
    /// delta and pull the value to that end of its range.
    #[test]
    fn only_the_viewport_edges_warp() {
        let rect = Rect::from_min_size(pos2(20.0, 30.0), vec2(800.0, 600.0));
        assert_eq!(wrap_target(pos2(20.5, 500.0), rect), Some(pos2(817.0, 500.0)));
        assert_eq!(wrap_target(pos2(819.5, 500.0), rect), Some(pos2(23.0, 500.0)));
        assert_eq!(wrap_target(pos2(19.0, 500.0), rect), Some(pos2(817.0, 500.0)));
        assert_eq!(wrap_target(pos2(20.5, 631.0), rect), None);
        assert_eq!(wrap_target(rect.center(), rect), None, "no warp away from the edges, however far the drag has gone");
        assert_eq!(wrap_target(pos2(36.0, 500.0), rect), None, "16 pt from the press point must not warp");
        assert_eq!(wrap_target(pos2(0.5, 10.0), Rect::from_min_size(Pos2::ZERO, vec2(8.0, 20.0))), None);
    }
}
