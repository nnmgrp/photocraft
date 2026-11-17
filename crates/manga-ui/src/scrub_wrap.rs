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
//!
//! Three guards keep that jump out of the value. A warp needs a real crossing since the previous
//! frame ([`warp_crossed`]), so a widget that sits at an edge is not warped on the frame it is
//! grabbed. A horizontal delta too long for a hand ([`plausible_step`]) is dropped rather than
//! applied. And one frame may only move the value half its range ([`bounded_change`]).

use egui::{Pos2, Rect, Response, Ui, Vec2};

#[derive(Clone, Copy)]
struct Scrub {
    precise: f64,
    confined: bool,
    pending: Option<(Pos2, Pos2, u8)>,
    /// Where the pointer was on the previous frame of this drag.
    prev: Option<Pos2>,
}

const MAX_FRAME_SCRUB_PT: f32 = 64.0;
const PENDING_WARP_TTL: u8 = 6;

fn clamp_frame_delta(delta: Vec2) -> Vec2 {
    if !delta.is_finite() {
        return Vec2::ZERO;
    }
    Vec2::new(delta.x.clamp(-MAX_FRAME_SCRUB_PT, MAX_FRAME_SCRUB_PT), delta.y.clamp(-MAX_FRAME_SCRUB_PT, MAX_FRAME_SCRUB_PT))
}

pub fn active(ui: &Ui) -> bool {
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

/// The warp for a drag that reached an edge on this frame, having been inside it on the frame
/// before.
///
/// Requiring the crossing keeps a pointer that merely sits at an edge from warping on the frame it
/// is grabbed: the frame the drag starts on compares against the press point, which is at that edge
/// too. A drag that starts inside and then reaches the edge still wraps, so a long scrub keeps
/// going, and a pointer that lands on the opposite edge does not bounce there and back.
fn warp_crossed(prev: Option<Pos2>, pos: Pos2, viewport: Rect) -> Option<Pos2> {
    let prev = prev.filter(|p| p.is_finite())?;
    let to = wrap_target(pos, viewport)?;
    let crossed_left = prev.x > viewport.left() + 1.0 && pos.x <= viewport.left() + 1.0;
    let crossed_right = prev.x < viewport.right() - 1.0 && pos.x >= viewport.right() - 1.0;
    (crossed_left || crossed_right).then_some(to)
}

/// Whether a frame's horizontal motion is short enough for a hand to have made.
///
/// A warp the OS applies lands whole in that frame's delta — [`delta`](egui::PointerState::delta) is
/// `latest_pos - old_pos` — and an uncompensated jump spans most of the window. Treating such a
/// frame as motion is what used to drive the value to an end of its range: the whole jump became
/// one `change`, and the clamp then held the value there. A frame that large is dropped instead, so
/// a compensation that fails costs one frame rather than the value.
fn plausible_step(dx: f32, viewport: Rect) -> bool {
    dx.is_finite() && viewport.is_finite() && dx.abs() < (viewport.width() * 0.5).max(64.0)
}

/// One frame's change, kept from crossing more than half the range.
///
/// The change is bounded, not just the value it lands on: however large a jump reaches the delta, a
/// single frame can never run the value from one end of its range to the other.
fn bounded_change(change: f64, lo: f64, hi: f64) -> f64 {
    if !change.is_finite() || lo > hi {
        return 0.0;
    }
    let limit = (hi - lo) * 0.5;
    change.clamp(-limit, limit)
}

/// [`delta`](Vec2) with the jump of a pending edge warp taken out.
///
/// The warp reaches the app through the OS, so the frame that carries it holds the whole jump in
/// its delta. Only the frame that actually put the pointer down at the warp's destination may be
/// corrected, and only by that jump: a pointer that turned back before the warp landed is still
/// where it looks, so its delta is already all real motion and subtracting the jump would inject
/// one. That injection ran the value to an end of its range.
fn unwarped(delta: Vec2, pos: Pos2, pending: &mut Option<(Pos2, Pos2, u8)>) -> Vec2 {
    let Some((from, to, ttl)) = *pending else { return delta };
    let step = to - from;
    if !from.is_finite() || !to.is_finite() || step.x == 0.0 || !pos.is_finite() {
        *pending = None;
        return delta;
    }
    let landed = (pos.x - to.x).abs() <= (step.x.abs() * 0.25).max(1.0);
    // The jump has to be in this frame's delta for taking it out to mean anything.
    let carries_jump = delta.x.abs() >= step.x.abs() * 0.5;
    if landed {
        *pending = None;
        if carries_jump { delta - step } else { delta }
    } else if ttl > 1 {
        // The OS warp is still in flight; keep `pending` across intermediate frames.
        *pending = Some((from, to, ttl - 1));
        delta
    } else {
        *pending = None;
        delta
    }
}

pub fn is_holding(ui: &Ui, resp: &Response) -> bool {
    ui.data(|d| d.get_temp::<Scrub>(resp.id.with("wrapped-scrub"))).is_some_and(|s| s.confined)
}

pub fn release(ui: &Ui, id: egui::Id) {
    let key = id.with("wrapped-scrub");
    if ui.data(|d| d.get_temp::<Scrub>(key)).is_some_and(|s| s.confined) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
    }
    ui.data_mut(|d| d.remove::<Scrub>(key));
}

/// Tracks cursor confinement and warp compensation for an active drag `resp`.
/// Returns `(unwarped_delta, holding)`.
pub fn drag_wrap(ui: &Ui, resp: &Response) -> (Vec2, bool) {
    let key = resp.id.with("wrapped-scrub");
    let dragging = active(ui) && resp.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.pointer.primary_down());
    if !dragging {
        release(ui, resp.id);
        return (Vec2::ZERO, false);
    }
    let (pos, delta, motion, viewport) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.delta(), i.pointer.motion(), i.viewport_rect()));
    let Some(pos) = pos else {
        let raw = motion.filter(|m| m.is_finite()).unwrap_or(delta);
        return (clamp_frame_delta(raw), false);
    };
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub { precise: 0.0, confined: false, pending: None, prev: None });
    if !scrub.confined {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
        scrub.confined = true;
    }
    // On the first frame of a drag the previous position is the press point, not the position the
    // pointer has already reached: a widget that sits against a window edge must not warp on the
    // frame it is grabbed there, which is what flew the cursor across the window and ran the
    // value to an end of its range.
    if scrub.prev.is_none() {
        scrub.prev = ui.input(|i| i.pointer.press_origin()).filter(|p| p.is_finite()).or(Some(pos));
    }
    let delta = unwarped(delta, pos, &mut scrub.pending);
    // A jump this long is a warp that escaped compensation, not a hand: leave the value alone and
    // keep the drag alive from here.
    let fallback = if plausible_step(delta.x, viewport) { delta } else { Vec2::new(0.0, delta.y) };
    let step = clamp_frame_delta(motion.filter(|m| m.is_finite()).unwrap_or(fallback));
    if scrub.pending.is_none()
        && let Some(to) = warp_crossed(scrub.prev, pos, viewport)
    {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::CursorPosition(to));
        scrub.pending = Some((pos, to, PENDING_WARP_TTL));
        ui.ctx().request_repaint();
    }
    scrub.prev = Some(pos);
    ui.data_mut(|d| d.insert_temp(key, scrub));
    (step, true)
}

pub fn update(ui: &Ui, resp: &Response, value: &mut f32, before: f32, range: std::ops::RangeInclusive<f32>, speed: f64, wrapping: bool) {
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
    let mut scrub = ui.data(|d| d.get_temp::<Scrub>(key)).unwrap_or(Scrub { precise: f64::from(before), confined: true, pending: None, prev: None });
    if !had_scrub || !scrub.precise.is_finite() {
        scrub.precise = f64::from(before);
    }
    let speed = if slow { speed / 10.0 } else { speed };
    let (lo, hi) = (f64::from(*range.start()), f64::from(*range.end()));
    let delta = clamp_frame_delta(delta);
    let change = bounded_change(f64::from(delta.x - delta.y) * speed, lo, hi);
    if change != 0.0 && lo <= hi {
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

    /// The press frame has no previous position, so a pointer that sits at a viewport edge is
    /// never warped when the drag is grabbed. That press-frame warp is what flew the cursor to
    /// the far edge and drove the value to an end of its range.
    #[test]
    fn the_press_frame_never_warps() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        assert_eq!(warp_crossed(None, pos2(799.5, 300.0), rect), None, "no previous position means no crossing");
        assert_eq!(warp_crossed(None, pos2(0.5, 300.0), rect), None, "the same at the left edge");
        assert_eq!(warp_crossed(Some(pos2(799.5, 300.0)), pos2(799.5, 300.0), rect), None, "standing still at an edge is not a crossing");
        assert_eq!(
            warp_crossed(Some(pos2(799.5, 300.0)), pos2(800.0, 300.0), rect),
            None,
            "a drag grabbed at the edge and pushed further does not warp either"
        );
    }

    /// Reaching an edge from inside the viewport warps, so a long scrub keeps going.
    #[test]
    fn reaching_an_edge_from_inside_warps() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        assert_eq!(warp_crossed(Some(pos2(700.0, 300.0)), pos2(799.5, 300.0), rect), Some(pos2(3.0, 300.0)), "the right edge wraps to the left one");
        assert_eq!(warp_crossed(Some(pos2(100.0, 300.0)), pos2(0.5, 300.0), rect), Some(pos2(797.0, 300.0)), "the left edge wraps to the right one");
        assert_eq!(warp_crossed(Some(pos2(400.0, 300.0)), pos2(410.0, 300.0), rect), None, "no edge, no warp");
    }

    /// A warp that lands takes its jump back out of the frame's delta; the real motion stays.
    #[test]
    fn a_landed_warp_keeps_the_real_motion() {
        let from = pos2(799.5, 300.0);
        let to = pos2(3.0, 300.0);
        let motion = vec2(-2.0, 1.0);
        let mut pending = Some((from, to, PENDING_WARP_TTL));
        assert_eq!(unwarped(to - from + motion, to + motion, &mut pending), motion);
        assert!(pending.is_none());
    }

    /// An early frame that arrives before the OS delivers the warp leaves the delta untouched and
    /// retains the pending warp across intermediate frames, expiring after `PENDING_WARP_TTL`.
    #[test]
    fn a_frame_that_never_landed_is_left_alone() {
        let from = pos2(799.5, 300.0);
        let to = pos2(3.0, 300.0);
        let mut pending = Some((from, to, PENDING_WARP_TTL));
        let delta = vec2(-6.0, 0.5);
        for _ in 0..PENDING_WARP_TTL {
            assert_eq!(unwarped(delta, pos2(793.5, 300.5), &mut pending), delta);
        }
        assert!(pending.is_none());
    }

    /// An intermediate render frame arriving before Windows delivers `SetCursorPos` must NOT drop
    /// `pending`, otherwise the actual warp jump in the next frame would slam the value negative.
    #[test]
    fn pending_warp_is_not_dropped_on_early_frame() {
        let from = pos2(799.5, 300.0);
        let to = pos2(3.0, 300.0);
        let mut pending = Some((from, to, PENDING_WARP_TTL));
        let early_delta = vec2(1.0, 0.0);
        assert_eq!(unwarped(early_delta, pos2(799.5, 300.0), &mut pending), early_delta);
        assert!(pending.is_some(), "pending warp must survive early frames before OS warp arrives");

        let real_motion = vec2(2.0, 0.0);
        let compensated = unwarped((to - from) + real_motion, to + real_motion, &mut pending);
        assert_eq!(compensated, real_motion);
        assert!(pending.is_none());
    }

    #[test]
    fn frame_delta_clamps_to_max_scrub_pt() {
        assert_eq!(clamp_frame_delta(vec2(-1500.0, 200.0)), vec2(-64.0, 64.0));
        assert_eq!(clamp_frame_delta(vec2(12.5, -3.0)), vec2(12.5, -3.0));
        assert_eq!(clamp_frame_delta(vec2(f32::NAN, 5.0)), Vec2::ZERO);
    }

    /// A jump longer than a hand can make is a warp that escaped compensation: it is dropped
    /// rather than applied, so a failed compensation costs one frame and not the value.
    #[test]
    fn an_implausible_jump_is_dropped() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        assert!(plausible_step(-9.0, rect), "ordinary motion");
        assert!(!plausible_step(-780.0, rect), "a warp across the window is not a hand");
    }

    /// One frame may not move the value across more than half its range, whatever reached the
    /// delta: a single frame can no longer run the value from one end of its range to the other.
    #[test]
    fn one_frame_cannot_cross_the_range() {
        assert_eq!(bounded_change(1000.0, 0.0, 10.0), 5.0);
        assert_eq!(bounded_change(-1000.0, 0.0, 10.0), -5.0);
        assert_eq!(bounded_change(1.0, 0.0, 10.0), 1.0);
        assert_eq!(bounded_change(f64::NAN, 0.0, 10.0), 0.0);
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
