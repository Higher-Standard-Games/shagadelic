//! Where a roaming bot looks, from TypeScript `bot/view/BotLook.ts`. Out of a
//! fight the view rests a little below the way the bot runs, at the floor
//! ahead. Running, it glances now and then toward the most open direction
//! within 45 degrees of its run, a sight line down a hall rather than the
//! wall a route corner points at. In passing it looks at a major item's spot
//! it has not seen for 8 s. Glances turn only the view; movement keeps its
//! heading.

use std::f64::consts::{FRAC_PI_2, PI};

use crate::games::washoi::{
    Vec3, agents::player::combat::aim::wrap, perception::sight::Sight, random::Random,
};

const DEG: f64 = PI / 180.0;
/// Calm, the view rests this far below where the bot looks, never past 45.
pub(in crate::games::washoi) const CALM_PITCH: f64 = 10.0 * DEG;
pub(in crate::games::washoi) const CALM_MAX_PITCH: f64 = 45.0 * DEG;
/// Sight lines are scored out to this far, and also 10 degrees up, so
/// stairs ahead are not a wall.
const LOOK_RANGE: f64 = 1500.0;
const LOOK_UP_PITCH: f64 = 10.0 * DEG;
/// How long a glance holds (±30%), and the rest between glances (from 0.67
/// to 1.33 of it), in server ticks.
const GLANCE_TICKS: f64 = 85.0;
const GLANCE_REST_TICKS: f64 = 375.0;
/// Looking the full 45 degrees aside costs this, of a score out of 1, and
/// turning the view a quarter turn from where it is costs this.
const LOOK_OFF_COST: f64 = 0.3;
const LOOK_MAX_OFF_TRAVEL: f64 = 45.0 * DEG;
/// While sprinting, a glance stays this close to the run. The movement keys
/// pick forward alone within 22.5 degrees of the view, and Washoi gives the
/// 400 u/s sprint only to forward alone; a person sprinting looks where it
/// runs too. TypeScript sends its feet's direction apart from its view,
/// which no client can do, so it glances the full 45 degrees at full speed.
const SPRINT_MAX_OFF_TRAVEL: f64 = 20.0 * DEG;
const LOOK_TURN_COST: f64 = 0.3;
/// How much more open another direction must be to take over.
const LOOK_SWITCH_MARGIN: f64 = 0.2;
const LOOK_STEP: f64 = 15.0 * DEG;
/// With nothing worth a glance, look again this soon.
const LOOK_INTERVAL_TICKS: u32 = 50;
/// A major item's spot unseen this long is worth a look in passing.
const SPOT_STALE_TICKS: u32 = 1000;
/// Spots are looked at from this far to as far as perception tells them
/// apart, and count as seen within the view's 55 degrees.
const SPOT_MIN_DISTANCE: f64 = 200.0;
const SPOT_MAX_DISTANCE: f64 = 2000.0;
const VIEW_HALF_ANGLE: f64 = 55.0 * DEG;
/// From a standing origin, the eye is this far up and the floor this far down.
const EYE_HEIGHT: f64 = 32.0;
const STANDING_LIFT: f64 = 24.0;

struct Glance {
    off: f64,
    until: u32,
    /// A look at a major item's spot: its pickup ID, where it is, and since when.
    spot: Option<(usize, [f64; 3], u32)>,
}

#[derive(Default)]
pub(in crate::games::washoi) struct Look {
    glance: Option<Glance>,
    /// By pickup ID: when a look at its spot last began, and when the spot
    /// was last in sight.
    spot_looks: Vec<(usize, u32)>,
    spot_seen: Vec<(usize, u32)>,
}

/// The tick for `id` in a small map, if any.
fn when(list: &[(usize, u32)], id: usize) -> Option<u32> {
    list.iter()
        .find(|(known, _)| *known == id)
        .map(|(_, at)| *at)
}

fn set(list: &mut Vec<(usize, u32)>, id: usize, tick: u32) {
    match list.iter_mut().find(|(known, _)| *known == id) {
        Some(entry) => entry.1 = tick,
        None => list.push((id, tick)),
    }
}

impl Look {
    /// Out of a roaming run, the glance ends.
    pub(in crate::games::washoi) fn stop(&mut self) {
        self.glance = None;
    }

    /// Notes which major item spots are in sight: within 2,000 units, inside
    /// the view's 55 degrees, and visible from the eye.
    pub(in crate::games::washoi) fn watch(
        &mut self,
        tick: u32,
        position: Vec3,
        view_yaw: f64,
        spots: &[(usize, [f64; 3])],
        sight: &Sight,
    ) {
        for &(id, at) in spots {
            if in_sight(position, view_yaw, at, sight) {
                set(&mut self.spot_seen, id, tick);
            }
        }
    }

    /// `lookIntoOpenSpace`: the view yaw for a run along `travel`, looking
    /// on from `base` (the route's look ahead), from a view now at
    /// `view_yaw`. Glances stay within 90 degrees of the run.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::games::washoi) fn glance(
        &mut self,
        tick: u32,
        position: Vec3,
        base: f64,
        travel: f64,
        view_yaw: f64,
        spots: &[(usize, [f64; 3])],
        sight: &Sight,
        random: &mut Random,
    ) -> f64 {
        let allowed = |off: f64| wrap(base + off - travel).abs() <= FRAC_PI_2;
        if let Some(glance) = self.glance.as_mut()
            && let Some((id, at, since)) = glance.spot
        {
            // A look at an item's spot follows it, and ends once it is seen.
            glance.off = wrap(bearing(position, at) - base);
            if when(&self.spot_seen, id).is_some_and(|seen| seen > since) {
                glance.until = tick;
            }
        }
        if self.glance.as_ref().is_none_or(|glance| glance.off == 0.0)
            && let Some((id, at)) = self.spot_to_check(tick, position, base, &allowed, spots, sight)
        {
            set(&mut self.spot_looks, id, tick);
            self.glance = Some(Glance {
                off: wrap(bearing(position, at) - base),
                until: tick + GLANCE_TICKS as u32,
                spot: Some((id, at, tick)),
            });
        }
        if let Some(glance) = &self.glance
            && glance.off != 0.0
            && (tick >= glance.until || !allowed(glance.off))
        {
            // A glance ends: back to where the bot runs, for a rest.
            let rest = GLANCE_REST_TICKS * (0.67 + 0.67 * random.next());
            self.glance = Some(Glance {
                off: 0.0,
                until: tick + rest as u32,
                spot: None,
            });
        }
        if self
            .glance
            .as_ref()
            .is_none_or(|glance| tick >= glance.until)
        {
            let eye = [position.x, position.y + EYE_HEIGHT, position.z];
            // Turning from where the view is costs too, so a route corner
            // does not swing the view across.
            let score = |off: f64| {
                let yaw = base + off;
                open_fraction(sight, eye, yaw)
                    - LOOK_OFF_COST * off.abs() / LOOK_MAX_OFF_TRAVEL
                    - LOOK_TURN_COST * wrap(yaw - view_yaw).abs() / FRAC_PI_2
            };
            let mut best_off = 0.0;
            let mut best_score = score(0.0) + LOOK_SWITCH_MARGIN;
            let steps = (LOOK_MAX_OFF_TRAVEL / LOOK_STEP).round() as i32;
            for step in -steps..=steps {
                let off = f64::from(step) * LOOK_STEP;
                if step == 0 || !allowed(off) {
                    continue;
                }
                let value = score(off);
                if value > best_score {
                    best_score = value;
                    best_off = off;
                }
            }
            let until = if best_off == 0.0 {
                tick + LOOK_INTERVAL_TICKS
            } else {
                tick + (GLANCE_TICKS * (0.7 + 0.6 * random.next())) as u32
            };
            self.glance = Some(Glance {
                off: best_off,
                until,
                spot: None,
            });
        }
        base + self.glance.as_ref().map_or(0.0, |glance| glance.off)
    }

    /// `spotToCheck`: the longest-unseen major item spot, unseen for 8 s and
    /// not looked at for as long, within 200 to 2,000 units, in sight, and
    /// within a quarter turn of the run.
    fn spot_to_check(
        &self,
        tick: u32,
        position: Vec3,
        base: f64,
        allowed: &dyn Fn(f64) -> bool,
        spots: &[(usize, [f64; 3])],
        sight: &Sight,
    ) -> Option<(usize, [f64; 3])> {
        let stale = |at: Option<u32>| at.is_none_or(|at| tick.wrapping_sub(at) >= SPOT_STALE_TICKS);
        spots
            .iter()
            .filter(|(id, _)| {
                stale(when(&self.spot_seen, *id)) && stale(when(&self.spot_looks, *id))
            })
            .filter(|(_, at)| {
                let d = distance(position, *at);
                (SPOT_MIN_DISTANCE..=SPOT_MAX_DISTANCE).contains(&d)
                    && allowed(wrap(bearing(position, *at) - base))
                    && sight.clear(eye(position), spot_target(*at))
            })
            .min_by_key(|(id, _)| when(&self.spot_seen, *id).map_or(-1, i64::from))
            .copied()
    }
}

/// How open the view is toward `yaw`: the reach of a level ray and one 10
/// degrees up, out to 1,500 units.
fn open_fraction(sight: &Sight, eye: [f64; 3], yaw: f64) -> f64 {
    [0.0, LOOK_UP_PITCH]
        .into_iter()
        .map(|pitch: f64| {
            let reach = LOOK_RANGE * pitch.cos();
            let end = [
                eye[0] + yaw.sin() * reach,
                eye[1] + LOOK_RANGE * pitch.sin(),
                eye[2] + yaw.cos() * reach,
            ];
            sight.reach(eye, end)
        })
        .fold(0.0, f64::max)
}

fn in_sight(position: Vec3, view_yaw: f64, at: [f64; 3], sight: &Sight) -> bool {
    distance(position, at) <= SPOT_MAX_DISTANCE
        && wrap(bearing(position, at) - view_yaw).abs() <= VIEW_HALF_ANGLE
        && sight.clear(eye(position), spot_target(at))
}

fn eye(position: Vec3) -> [f64; 3] {
    [position.x, position.y + EYE_HEIGHT, position.z]
}

/// Where sight aims at an item spot given as a standing origin: the floor
/// under it, plus 0.8 of the eye height, as `canSeeFrom` aims at a target.
fn spot_target(at: [f64; 3]) -> [f64; 3] {
    [at[0], at[1] - STANDING_LIFT + 0.8 * EYE_HEIGHT, at[2]]
}

fn bearing(from: Vec3, to: [f64; 3]) -> f64 {
    (to[0] - from.x).atan2(to[2] - from.z)
}

fn distance(a: Vec3, b: [f64; 3]) -> f64 {
    (a.x - b[0]).hypot(a.z - b[2]).hypot(a.y - b[1])
}

/// A glance while sprinting, kept within reach of the forward key.
pub(in crate::games::washoi) fn keep_sprint(yaw: f64, travel: f64) -> f64 {
    travel + wrap(yaw - travel).clamp(-SPRINT_MAX_OFF_TRAVEL, SPRINT_MAX_OFF_TRAVEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::perception::sight::tests::{cube, encode};

    #[test]
    fn glances_into_the_open_side_and_back_to_the_run() {
        // Running toward +z, a wall 300 units ahead ends just right of the
        // run, so the open line is to the front right (+x).
        let sight = Sight::parse(&encode(
            "test",
            &[cube([-2000.0, -100.0, 300.0], [100.0, 400.0, 320.0])],
            &[],
        ))
        .unwrap();
        let me = Vec3::new(0.0, 24.0, 0.0);
        let mut look = Look::default();
        let mut random = Random::new(4);
        // Straight ahead is walled at 300; 30 degrees right is open.
        let yaw = look.glance(0, me, 0.0, 0.0, 0.0, &[], &sight, &mut random);
        assert!(yaw > 0.0 && yaw <= LOOK_MAX_OFF_TRAVEL + 1e-9, "{yaw}");
        // After the glance it comes back to the run for a rest.
        let mut back = None;
        for tick in 1..200 {
            let yaw = look.glance(tick, me, 0.0, 0.0, yaw, &[], &sight, &mut random);
            if yaw == 0.0 {
                back = Some(tick);
                break;
            }
        }
        assert!(
            back.is_some_and(|tick| (59..=111).contains(&tick)),
            "{back:?}"
        );
    }

    #[test]
    fn checks_a_stale_major_item_spot_in_passing() {
        let sight = Sight::parse(&encode("test", &[cube([5000.0; 3], [5001.0; 3])], &[])).unwrap();
        let me = Vec3::new(0.0, 24.0, 0.0);
        let red_armor = [(7, [600.0, 24.0, 600.0])];
        let mut look = Look::default();
        let mut random = Random::new(5);
        // Never seen: the bot looks at it, 45 degrees right of its run.
        let yaw = look.glance(2000, me, 0.0, 0.0, 0.0, &red_armor, &sight, &mut random);
        assert!((yaw - PI / 4.0).abs() < 1e-9);
        // Seen now, so that look ends at once; a spot seen lately is left be.
        look.watch(2001, me, yaw, &red_armor, &sight);
        let mut fresh = Look::default();
        fresh.watch(2000, me, PI / 4.0, &red_armor, &sight);
        let yaw = fresh.glance(2001, me, 0.0, 0.0, 0.0, &red_armor, &sight, &mut random);
        assert_eq!(yaw, 0.0);
    }

    #[test]
    fn a_sprinting_glance_stays_on_the_forward_key() {
        let travel = 1.0;
        assert!((keep_sprint(travel + 40.0 * DEG, travel) - (travel + 20.0 * DEG)).abs() < 1e-9);
        assert!((keep_sprint(travel - 10.0 * DEG, travel) - (travel - 10.0 * DEG)).abs() < 1e-9);
        // Across the wrap: 350 degrees off is 10 degrees the other way.
        let back = keep_sprint(travel + 350.0 * DEG, travel);
        assert!((back - (travel - 10.0 * DEG)).abs() < 1e-9);
    }
}
