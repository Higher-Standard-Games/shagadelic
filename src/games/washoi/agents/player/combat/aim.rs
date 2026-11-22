//! Port of TypeScript `bot/BotAimController.ts`: the view follows the wanted
//! angles on a critically damped spring with a top speed. Big turns wait for
//! the reaction time and sometimes overshoot, a slowly wandering offset stands
//! in for aim error, and a target under fire is pursued. Out of a fight the
//! goal eases toward the wanted view. It runs once per command sent.

use std::f64::consts::PI;

use crate::games::washoi::{difficulty::AimSkill, random::Random};

const DEG: f64 = PI / 180.0;
const YAW_SPEED_PER_TRACKING: f64 = 100.0 * DEG;
const PITCH_SPEED_SCALE: f64 = 0.6;
const BASE_SETTLE_SECONDS: f64 = 0.5;
const SETTLE_OMEGA: f64 = 4.74;
const REACTION_ANGLE: f64 = 45.0 * DEG;
const REACTION_SPREAD: f64 = 0.25;
const OVERSHOOT_BASE: f64 = 0.15;
const OVERSHOOT_PER_MISS: f64 = 0.35;
const OVERSHOOT_MIN: f64 = 0.04;
const OVERSHOOT_MAX: f64 = 0.12;
const OVERSHOOT_REACHED: f64 = 2.0 * DEG;
const OVERSHOOT_HOLD: f64 = 1.0;
const DRIFT_SECONDS: f64 = 1.0;
const DRIFT_GLIDE_SECONDS: f64 = 0.3;
const NOISE_WIDTH: f64 = 0.15;
const MAX_PITCH: f64 = 89.0 * DEG;
const PURSUIT_BLEND: f64 = 0.5;
const PURSUIT_MAX_STEP: f64 = 10.0 * DEG;
const PURSUIT_STALE_SECONDS: f64 = 0.1;
const CALM_EASE_SECONDS: f64 = 0.6;
const CALM_MAX_GOAL_SPEED: f64 = 120.0 * DEG;
const CALM_MAX_EASED_TURN: f64 = 90.0 * DEG;
const CALM_DEADZONES: f64 = 5.0;
const CALM_FINISH: f64 = 0.25;
/// Brisk calm, for a bot running a route with nobody about: a person turns
/// to the next leg quickly. The movement keys follow the view sent, so a
/// view trailing the run sends a diagonal, and a diagonal caps the run at
/// 320. TypeScript's feet don't follow its view, so it eases at 0.6 s.
const BRISK_EASE_SECONDS: f64 = 0.12;
const BRISK_MAX_GOAL_SPEED: f64 = 360.0 * DEG;
const DEADZONE_POINTS: [(f64, f64); 4] = [
    (1.5, 4.0 * DEG),
    (3.0, 3.0 * DEG),
    (5.0, 2.0 * DEG),
    (8.0, 1.5 * DEG),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Tuning {
    max_yaw_speed: f64,
    max_pitch_speed: f64,
    settle_seconds: f64,
    deadzone: f64,
    reaction_ms: f64,
    drift_sigma: f64,
}

impl Tuning {
    /// `aimTuning`.
    pub(in crate::games::washoi) fn new(skill: AimSkill) -> Self {
        let tracking = skill.tracking.max(0.1);
        let max_yaw_speed = YAW_SPEED_PER_TRACKING * tracking;
        let missing = (1.0 - skill.accuracy).clamp(0.0, 1.0);
        Self {
            max_yaw_speed,
            max_pitch_speed: max_yaw_speed * PITCH_SPEED_SCALE,
            settle_seconds: BASE_SETTLE_SECONDS / tracking.sqrt(),
            deadzone: deadzone_for(tracking),
            reaction_ms: skill.reaction_ms.max(0.0),
            drift_sigma: NOISE_WIDTH * missing / 12f64.sqrt(),
        }
    }
}

fn deadzone_for(tracking: f64) -> f64 {
    let points = DEADZONE_POINTS;
    if tracking <= points[0].0 {
        return points[0].1;
    }
    for pair in points.windows(2) {
        let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
        if tracking <= x1 {
            return y0 + (y1 - y0) * (tracking - x0) / (x1 - x0);
        }
    }
    points[points.len() - 1].1
}

/// An angle wrapped to (−π, π].
pub(in crate::games::washoi) fn wrap(angle: f64) -> f64 {
    angle.sin().atan2(angle.cos())
}

#[derive(Clone, Copy, Debug)]
struct Axis {
    value: f64,
    velocity: f64,
    goal: f64,
    offset: f64,
    drift_target: f64,
    drift_left: f64,
    wanted: f64,
    held_for: f64,
    pursuit: f64,
    easing: bool,
}

impl Axis {
    fn new(value: f64) -> Self {
        Self {
            value,
            velocity: 0.0,
            goal: value,
            offset: 0.0,
            drift_target: 0.0,
            drift_left: 0.0,
            wanted: value,
            held_for: 0.0,
            pursuit: 0.0,
            easing: false,
        }
    }

    /// `track`: follow how fast the wanted angle moves.
    fn track(&mut self, wanted: f64, dt: f64) {
        self.held_for += dt;
        let change = wrap(wanted - self.wanted);
        if change == 0.0 {
            if self.held_for > PURSUIT_STALE_SECONDS {
                self.pursuit = 0.0;
            }
            return;
        }
        if change.abs() > PURSUIT_MAX_STEP {
            self.pursuit = 0.0;
        } else {
            self.pursuit += (change / self.held_for - self.pursuit) * PURSUIT_BLEND;
        }
        self.wanted = wanted;
        self.held_for = 0.0;
    }

    /// `follow`: one step of a critically damped spring toward a moving
    /// target, under a top speed.
    fn follow(
        &mut self,
        target: f64,
        target_speed: f64,
        max_speed: f64,
        t: &Tuning,
        dt: f64,
    ) -> f64 {
        let omega = SETTLE_OMEGA / t.settle_seconds;
        let x = omega * dt;
        let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
        let error = self.value - target;
        let relative = self.velocity - target_speed;
        let temp = (relative + omega * error) * dt;
        let mut velocity = (relative - omega * temp) * decay + target_speed;
        let mut next = target + target_speed * dt + (error + temp) * decay;
        if velocity.abs() > max_speed {
            velocity = velocity.signum() * max_speed;
        }
        let moved = next - self.value;
        if moved.abs() > max_speed * dt {
            next = self.value + moved.signum() * max_speed * dt;
        }
        self.velocity = velocity;
        next
    }
}

struct Pending {
    yaw: f64,
    pitch: f64,
    remaining: f64,
}

pub(in crate::games::washoi) struct AimController {
    tuning: Tuning,
    accuracy: f64,
    view: Option<(Axis, Axis)>,
    pending: Option<Pending>,
    overshoot: Option<f64>,
    /// Calm turns at the brisk pace.
    brisk: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct View {
    pub(in crate::games::washoi) yaw: f64,
    pub(in crate::games::washoi) pitch: f64,
}

impl AimController {
    pub(in crate::games::washoi) fn new(skill: AimSkill) -> Self {
        Self {
            tuning: Tuning::new(skill),
            accuracy: skill.accuracy.clamp(0.0, 1.0),
            view: None,
            pending: None,
            overshoot: None,
            brisk: false,
        }
    }

    /// Calm turns at the brisk pace of a person running a route alone.
    pub(in crate::games::washoi) fn set_brisk(&mut self, brisk: bool) {
        self.brisk = brisk;
    }

    /// `setDifficulty`: new rates from here on, keeping the view.
    pub(in crate::games::washoi) fn set_skill(&mut self, skill: AimSkill) {
        self.tuning = Tuning::new(skill);
        self.accuracy = skill.accuracy.clamp(0.0, 1.0);
    }

    /// The view as last sent, if any.
    pub(in crate::games::washoi) fn current(&self) -> Option<View> {
        self.view.map(|(yaw, pitch)| View {
            yaw: yaw.value,
            pitch: pitch.value,
        })
    }

    /// Forget the view, as on death.
    pub(in crate::games::washoi) fn clear(&mut self) {
        self.view = None;
        self.pending = None;
        self.overshoot = None;
    }

    /// Look exactly here, at rest: after a script set the view itself.
    pub(in crate::games::washoi) fn reset(&mut self, view: View) {
        self.view = Some((Axis::new(view.yaw), Axis::new(view.pitch)));
        self.pending = None;
        self.overshoot = None;
    }

    /// Advance by `dt` seconds toward `wanted` and return the view to send.
    pub(in crate::games::washoi) fn step(
        &mut self,
        dt: f64,
        wanted: View,
        firing: bool,
        calm: bool,
        random: &mut Random,
    ) -> View {
        if self.view.is_none() {
            self.reset(wanted);
        }
        let Some((mut yaw, mut pitch)) = self.view else {
            unreachable!()
        };
        let t = self.tuning;
        yaw.track(wanted.yaw, dt);
        pitch.track(wanted.pitch, dt);
        let eased = calm && !firing && wrap(wanted.yaw - yaw.goal).abs() <= CALM_MAX_EASED_TURN;
        if eased {
            self.ease_goal(&mut yaw, &mut pitch, dt, wanted);
        } else {
            self.choose_goal(&mut yaw, &mut pitch, dt, wanted, firing, random);
        }
        self.drift(&mut yaw, dt, random);
        self.drift(&mut pitch, dt, random);

        let pursue = firing && self.pending.is_none();
        let yaw_speed = if pursue { yaw.pursuit } else { 0.0 };
        let yaw_target =
            yaw.value + wrap(yaw.goal + yaw.offset + yaw_speed * yaw.held_for - yaw.value);
        yaw.value = wrap(yaw.follow(yaw_target, yaw_speed, t.max_yaw_speed, &t, dt));
        let pitch_speed = if pursue { pitch.pursuit } else { 0.0 };
        let pitch_target = clamp_pitch(pitch.goal + pitch.offset + pitch_speed * pitch.held_for);
        pitch.value =
            clamp_pitch(pitch.follow(pitch_target, pitch_speed, t.max_pitch_speed, &t, dt));
        self.view = Some((yaw, pitch));
        View {
            yaw: yaw.value,
            pitch: pitch.value,
        }
    }

    fn ease_goal(&mut self, yaw: &mut Axis, pitch: &mut Axis, dt: f64, wanted: View) {
        self.pending = None;
        self.overshoot = None;
        let deadzone = self.tuning.deadzone;
        let (seconds, speed) = if self.brisk {
            (BRISK_EASE_SECONDS, BRISK_MAX_GOAL_SPEED)
        } else {
            (CALM_EASE_SECONDS, CALM_MAX_GOAL_SPEED)
        };
        let ease = 1.0 - (-dt / seconds).exp();
        let cap = speed * dt;
        let yaw_off = wrap(wanted.yaw - yaw.goal);
        if yaw_off.abs() > deadzone * CALM_DEADZONES || yaw.easing {
            yaw.goal = wrap(yaw.goal + (yaw_off * ease).clamp(-cap, cap));
            yaw.easing = yaw_off.abs() > deadzone * CALM_FINISH;
        }
        let pitch_off = wanted.pitch - pitch.goal;
        if pitch_off.abs() > deadzone || pitch.easing {
            pitch.goal += (pitch_off * ease).clamp(-cap, cap);
            pitch.easing = pitch_off.abs() > deadzone * CALM_FINISH;
        }
    }

    fn choose_goal(
        &mut self,
        yaw: &mut Axis,
        pitch: &mut Axis,
        dt: f64,
        wanted: View,
        firing: bool,
        random: &mut Random,
    ) {
        let jump = wrap(wanted.yaw - yaw.goal)
            .abs()
            .max((wanted.pitch - pitch.goal).abs());
        if jump > REACTION_ANGLE && self.tuning.reaction_ms > 0.0 {
            let pending = self.pending.get_or_insert_with(|| Pending {
                yaw: wanted.yaw,
                pitch: wanted.pitch,
                remaining: self.tuning.reaction_ms / 1000.0
                    * (REACTION_SPREAD * standard_normal(random.next())).exp(),
            });
            pending.yaw = wanted.yaw;
            pending.pitch = wanted.pitch;
            pending.remaining -= dt;
            if pending.remaining > 0.0 {
                return;
            }
            let turn = wrap(wanted.yaw - yaw.goal);
            yaw.goal = wanted.yaw;
            pitch.goal = wanted.pitch;
            self.pending = None;
            self.overshoot = None;
            if random.next() < OVERSHOOT_BASE + OVERSHOOT_PER_MISS * (1.0 - self.accuracy) {
                let past = OVERSHOOT_MIN + (OVERSHOOT_MAX - OVERSHOOT_MIN) * random.next();
                yaw.goal = wrap(wanted.yaw + turn * past);
                self.overshoot = Some(0.0);
            }
            return;
        }
        self.pending = None;

        if let Some(held) = &mut self.overshoot {
            *held += dt;
            let arrived = wrap(yaw.goal + yaw.offset - yaw.value).abs() < OVERSHOOT_REACHED;
            if !arrived && *held < OVERSHOOT_HOLD {
                return;
            }
            self.overshoot = None;
            yaw.goal = wanted.yaw;
            pitch.goal = wanted.pitch;
            return;
        }

        let deadzone = if firing { 0.0 } else { self.tuning.deadzone };
        if wrap(wanted.yaw - yaw.goal).abs() > deadzone {
            yaw.goal = wanted.yaw;
        }
        if (wanted.pitch - pitch.goal).abs() > deadzone {
            pitch.goal = wanted.pitch;
        }
    }

    /// The aim error glides to a fresh random offset about once a second.
    fn drift(&self, axis: &mut Axis, dt: f64, random: &mut Random) {
        let sigma = self.tuning.drift_sigma;
        if sigma <= 0.0 {
            axis.offset = 0.0;
            return;
        }
        axis.drift_left -= dt;
        if axis.drift_left <= 0.0 {
            axis.drift_target = sigma * gaussian(random);
            axis.drift_left = DRIFT_SECONDS * (0.6 + 0.8 * random.next());
        }
        axis.offset +=
            (axis.drift_target - axis.offset) * (1.0 - (-dt / DRIFT_GLIDE_SECONDS).exp());
    }
}

fn gaussian(random: &mut Random) -> f64 {
    let u = random.next().max(1e-12);
    let v = random.next();
    (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
}

/// `standardNormal`: the logistic approximation of the normal quantile.
fn standard_normal(uniform: f64) -> f64 {
    let u = uniform.clamp(0.001, 0.999);
    0.5513 * (u / (1.0 - u)).ln()
}

fn clamp_pitch(pitch: f64) -> f64 {
    pitch.clamp(-MAX_PITCH, MAX_PITCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::Difficulty;

    const DT: f64 = 0.008;

    fn run(
        aim: &mut AimController,
        random: &mut Random,
        seconds: f64,
        wanted: View,
        firing: bool,
    ) -> Vec<View> {
        (0..(seconds / DT).round() as usize)
            .map(|_| aim.step(DT, wanted, firing, false, random))
            .collect()
    }

    #[test]
    fn tuning_matches_the_typescript_table() {
        let medium = Tuning::new(Difficulty::Medium.aim());
        assert!((medium.max_yaw_speed - 300.0 * DEG).abs() < 1e-9);
        assert!((medium.settle_seconds - 0.5 / 3f64.sqrt()).abs() < 1e-9);
        assert!((medium.deadzone - 3.0 * DEG).abs() < 1e-9);
        assert!((Tuning::new(Difficulty::Insane.aim()).deadzone - 1.5 * DEG).abs() < 1e-9);
        assert!((deadzone_for(4.0) - 2.5 * DEG).abs() < 1e-9);
    }

    #[test]
    fn big_turns_wait_for_the_reaction_and_respect_the_top_speed() {
        let skill = AimSkill {
            accuracy: 1.0,
            ..Difficulty::Medium.aim()
        };
        let mut aim = AimController::new(skill);
        let mut random = Random::new(0);
        aim.reset(View {
            yaw: 0.0,
            pitch: 0.0,
        });
        let wanted = View {
            yaw: PI / 2.0,
            pitch: 0.0,
        };
        // Long enough to come back from an overshoot.
        let views = run(&mut aim, &mut random, 3.0, wanted, true);
        // The clamped log-normal wait is at least 0.39 of the preset's 280 ms.
        assert!(views[..13].iter().all(|view| view.yaw.abs() < 1e-9));
        let speeds = views
            .windows(2)
            .map(|pair| (pair[1].yaw - pair[0].yaw).abs() / DT);
        assert!(speeds.fold(0.0, f64::max) <= 300.0 * DEG + 1e-6);
        assert!((views.last().unwrap().yaw - PI / 2.0).abs() < 2.0 * DEG);
    }

    #[test]
    fn small_corrections_track_at_once_and_wrap_around() {
        let mut aim = AimController::new(AimSkill {
            accuracy: 1.0,
            ..Difficulty::Hard.aim()
        });
        let mut random = Random::new(1);
        aim.reset(View {
            yaw: PI - 0.1,
            pitch: 0.0,
        });
        let wanted = View {
            yaw: -PI + 0.1,
            pitch: 0.2,
        };
        let views = run(&mut aim, &mut random, 0.5, wanted, true);
        // Across ±π the short way, with no reaction wait under 45°.
        assert!(views[0].yaw > PI - 0.1 || views[0].yaw < -PI + 0.2);
        let last = views.last().unwrap();
        assert!(wrap(last.yaw - wanted.yaw).abs() < 0.5 * DEG);
        assert!((last.pitch - 0.2).abs() < 0.5 * DEG);
    }

    #[test]
    fn calm_views_ease_and_ignore_small_changes() {
        let mut aim = AimController::new(Difficulty::Medium.aim());
        let mut random = Random::new(2);
        aim.reset(View {
            yaw: 0.0,
            pitch: 0.0,
        });
        let nudge = View {
            yaw: 10.0 * DEG,
            pitch: 0.0,
        };
        for _ in 0..100 {
            aim.step(DT, nudge, false, true, &mut random);
        }
        let (yaw, _) = aim.view.unwrap();
        assert_eq!(yaw.goal, 0.0, "under five deadzones");
        let turn = View {
            yaw: 60.0 * DEG,
            pitch: 0.0,
        };
        let mut last = 0.0;
        for _ in 0..25 {
            let view = aim.step(DT, turn, false, true, &mut random);
            assert!((view.yaw - last) / DT <= 300.0 * DEG + 1e-6);
            last = view.yaw;
        }
        let (yaw, _) = aim.view.unwrap();
        assert!(yaw.goal > 0.0 && yaw.goal <= 25.0 * DT * CALM_MAX_GOAL_SPEED + 1e-9);
    }

    #[test]
    fn a_brisk_calm_turn_reaches_the_next_leg_much_sooner() {
        // Ticks until a calm view is within the forward key's 22.5 degrees
        // of a 90 degree turn.
        let settle = |brisk: bool| {
            let mut aim = AimController::new(Difficulty::Hard.aim());
            aim.set_brisk(brisk);
            let mut random = Random::new(3);
            aim.reset(View {
                yaw: 0.0,
                pitch: 0.0,
            });
            let wanted = View {
                yaw: 90.0 * DEG,
                pitch: 0.0,
            };
            (1..=500)
                .find(|_| {
                    let view = aim.step(0.008, wanted, false, true, &mut random);
                    wrap(wanted.yaw - view.yaw).abs() < 22.5 * DEG
                })
                .unwrap_or(500)
        };
        let (calm, brisk) = (settle(false), settle(true));
        assert!(brisk * 2 < calm, "brisk {brisk} ticks, calm {calm}");
        assert!(brisk <= 50, "brisk {brisk} ticks");
    }
}
