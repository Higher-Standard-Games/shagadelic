//! The blade lunge, from TypeScript `bot/sequences/SlideBladeSequence.ts`:
//! a bot running at an enemy 200 to 450 units ahead crouch-slides at it
//! with the blade out, swings once in range, and hops aside to the gun it
//! had out when the slide fails.

use crate::games::washoi::{
    Vec3, agents::player::combat::FIRE, hosting::net::protocol::PlayerInput,
};

// `InputButtons` in `engine/network/components/InputComponent.ts`.
const FORWARD: u16 = 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;

pub(in crate::games::washoi) const LUNGE_MIN_DISTANCE: f64 = 200.0;
pub(in crate::games::washoi) const LUNGE_MAX_DISTANCE: f64 = 450.0;
const LUNGE_MAX_HEADING_OFF: f64 = 30.0 * std::f64::consts::PI / 180.0;
const LUNGE_MAX_RISE: f64 = 64.0;
const STRIKE_DISTANCE: f64 = 160.0;
const STRIKE_TICKS: u32 = 20;
const EXIT_TICKS: u32 = 12;
const SLIDE_CONFIRM_TICKS: u32 = 25;
/// `maxTicks`: a lunge lasts at most this long.
const MAX_TICKS: u32 = 180;
/// `sv_slideMinSpeed`.
const SLIDE_MIN_SPEED: f64 = 375.0;
const BLADE: &str = "blade";

/// The bot's own body as the lunge needs it.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Runner {
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) sliding: bool,
    pub(in crate::games::washoi) yaw: f64,
    pub(in crate::games::washoi) pitch: f64,
}

/// The enemy fought: where it is and how it moves.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Mark {
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
}

struct Running {
    target: String,
    started: u32,
    previous_weapon: String,
    was_sliding: bool,
    strike: Option<u32>,
    exit: Option<u32>,
    side: f64,
}

#[derive(Default)]
pub(in crate::games::washoi) struct Lunge {
    running: Option<Running>,
}

/// The 1-based slot of a weapon, 0 when not held.
fn slot_of(weapons: &[String], weapon: &str) -> u8 {
    weapons
        .iter()
        .position(|held| held == weapon)
        .map_or(0, |index| u8::try_from(index + 1).unwrap_or(0))
}

/// `canLunge`: grounded and not sliding yet, at slide speed, with the blade,
/// heading within 30 degrees of an enemy 200 to 450 units off, no more than
/// 64 above, on walkable ground between.
pub(in crate::games::washoi) fn can_lunge(
    me: Runner,
    target: Mark,
    weapons: &[String],
    walkable: impl Fn(Vec3, Vec3) -> bool,
) -> bool {
    if !me.grounded || me.sliding || slot_of(weapons, BLADE) == 0 {
        return false;
    }
    if me.velocity.x.hypot(me.velocity.z) < SLIDE_MIN_SPEED {
        return false;
    }
    let (dx, dz) = (
        target.position.x - me.position.x,
        target.position.z - me.position.z,
    );
    let distance = dx.hypot(dz);
    if !(LUNGE_MIN_DISTANCE..=LUNGE_MAX_DISTANCE).contains(&distance)
        || target.position.y - me.position.y > LUNGE_MAX_RISE
    {
        return false;
    }
    let off = dx.atan2(dz) - me.velocity.x.atan2(me.velocity.z);
    if off.sin().atan2(off.cos()).abs() > LUNGE_MAX_HEADING_OFF {
        return false;
    }
    walkable(me.position, target.position)
}

impl Lunge {
    pub(in crate::games::washoi) fn active(&self) -> bool {
        self.running.is_some()
    }

    pub(in crate::games::washoi) fn stop(&mut self) {
        self.running = None;
    }

    /// Start a lunge at the enemy fought, drawn by `side` for the hop out.
    pub(in crate::games::washoi) fn start(
        &mut self,
        tick: u32,
        target: &str,
        current_weapon: &str,
        left: bool,
    ) {
        self.running = Some(Running {
            target: target.to_owned(),
            started: tick,
            previous_weapon: current_weapon.to_owned(),
            was_sliding: false,
            strike: None,
            exit: None,
            side: if left { -1.0 } else { 1.0 },
        });
    }

    /// The command for this decision while lunging, or None once over.
    /// `mark` is the enemy lunged at, None once it is dead or gone.
    pub(in crate::games::washoi) fn step(
        &mut self,
        tick: u32,
        me: Runner,
        mark: impl Fn(&str) -> Option<Mark>,
        weapons: &[String],
        current_weapon: &str,
        prediction: f64,
    ) -> Option<PlayerInput> {
        let running = self.running.as_mut()?;
        if tick.saturating_sub(running.started) > MAX_TICKS {
            self.running = None;
            return None;
        }
        if running.exit.is_none() {
            match mark(&running.target) {
                Some(target) => {
                    if let Some(input) = lunge(
                        running,
                        tick,
                        me,
                        target,
                        weapons,
                        current_weapon,
                        prediction,
                    ) {
                        return Some(input);
                    }
                    if running.strike.is_some() && running.exit.is_none() {
                        // The swing is done: the lunge succeeded.
                        self.running = None;
                        return None;
                    }
                }
                None => running.exit = Some(tick),
            }
        }
        let running = self.running.as_mut()?;
        let exit = *running.exit.get_or_insert(tick);
        if tick.saturating_sub(exit) >= EXIT_TICKS {
            self.running = None;
            return None;
        }
        // Hop aside, back to the gun that was out.
        let (sin, cos) = me.yaw.sin_cos();
        let (right, forward) = (running.side, 0.5);
        let length = right.hypot(forward);
        let previous = &running.previous_weapon;
        Some(PlayerInput {
            direction: [
                (right * cos + forward * sin) / length,
                0.0,
                (forward * cos - right * sin) / length,
            ],
            yaw: me.yaw,
            pitch: me.pitch,
            buttons: JUMP | if running.side < 0.0 { LEFT } else { RIGHT },
            weapon_slot: if previous.is_empty() || previous == BLADE {
                0
            } else {
                slot_of(weapons, previous)
            },
        })
    }
}

/// The slide and the strike. None when the lunge ends here: the swing is
/// over, or the slide failed and the exit begins.
fn lunge(
    running: &mut Running,
    tick: u32,
    me: Runner,
    target: Mark,
    weapons: &[String],
    current_weapon: &str,
    prediction: f64,
) -> Option<PlayerInput> {
    let speed = me.velocity.x.hypot(me.velocity.z).max(SLIDE_MIN_SPEED);
    let distance = (target.position.x - me.position.x).hypot(target.position.z - me.position.z);
    let lead = prediction * distance / speed;
    let aim = Vec3::new(
        target.position.x + target.velocity.x * lead,
        target.position.y,
        target.position.z + target.velocity.z * lead,
    );
    let yaw = (aim.x - me.position.x).atan2(aim.z - me.position.z);
    let pitch = -(aim.y - me.position.y).atan2(distance.max(1.0));
    let blade = slot_of(weapons, BLADE);
    let run = |buttons: u16| PlayerInput {
        direction: [yaw.sin(), 0.0, yaw.cos()],
        yaw,
        pitch,
        buttons,
        weapon_slot: blade,
    };
    if me.sliding {
        running.was_sliding = true;
    }
    if let Some(strike) = running.strike {
        return (tick.saturating_sub(strike) < STRIKE_TICKS).then(|| run(FORWARD | FIRE));
    }
    if distance <= STRIKE_DISTANCE && current_weapon == BLADE {
        running.strike = Some(tick);
        return Some(run(FORWARD | CROUCH | FIRE));
    }
    let missed = running.was_sliding && !me.sliding;
    let unconfirmed =
        !running.was_sliding && tick.saturating_sub(running.started) >= SLIDE_CONFIRM_TICKS;
    if missed || unconfirmed {
        running.exit = Some(tick);
        return None;
    }
    Some(run(FORWARD | SPRINT | CROUCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(x: f64, speed: f64, sliding: bool) -> Runner {
        Runner {
            position: Vec3::new(x, 0.0, 0.0),
            velocity: Vec3::new(speed, 0.0, 0.0),
            grounded: true,
            sliding,
            yaw: std::f64::consts::FRAC_PI_2,
            pitch: 0.0,
        }
    }

    fn weapons() -> Vec<String> {
        vec!["blade".into(), "rlauncher".into()]
    }

    #[test]
    fn lunges_only_at_slide_speed_toward_a_close_enemy_ahead() {
        let target = Mark {
            position: Vec3::new(300.0, 0.0, 0.0),
            velocity: Vec3::default(),
        };
        let open = |_, _| true;
        assert!(can_lunge(
            runner(0.0, 400.0, false),
            target,
            &weapons(),
            open
        ));
        assert!(!can_lunge(
            runner(0.0, 300.0, false),
            target,
            &weapons(),
            open
        ));
        assert!(!can_lunge(
            runner(0.0, -400.0, false),
            target,
            &weapons(),
            open
        ));
        assert!(!can_lunge(
            runner(200.0, 400.0, false),
            target,
            &weapons(),
            open
        ));
        assert!(!can_lunge(
            runner(0.0, 400.0, false),
            target,
            &["rlauncher".into()],
            open
        ));
        assert!(!can_lunge(
            runner(0.0, 400.0, false),
            target,
            &weapons(),
            |_, _| false
        ));
    }

    #[test]
    fn slides_strikes_in_range_and_ends() {
        let mut lunge = Lunge::default();
        let target = |at: f64| {
            move |_: &str| {
                Some(Mark {
                    position: Vec3::new(at, 0.0, 0.0),
                    velocity: Vec3::default(),
                })
            }
        };
        lunge.start(0, "foe", "rlauncher", true);
        let slide = lunge
            .step(
                1,
                runner(0.0, 400.0, false),
                target(300.0),
                &weapons(),
                "rlauncher",
                0.5,
            )
            .unwrap();
        assert_eq!(slide.buttons, FORWARD | SPRINT | CROUCH);
        assert_eq!(slide.weapon_slot, 1);
        assert!((slide.direction[0] - 1.0).abs() < 1e-9);
        lunge.step(
            5,
            runner(100.0, 500.0, true),
            target(300.0),
            &weapons(),
            "blade",
            0.5,
        );
        let strike = lunge
            .step(
                10,
                runner(160.0, 500.0, true),
                target(300.0),
                &weapons(),
                "blade",
                0.5,
            )
            .unwrap();
        assert_eq!(strike.buttons, FORWARD | CROUCH | FIRE);
        let swing = lunge
            .step(
                20,
                runner(200.0, 300.0, false),
                target(300.0),
                &weapons(),
                "blade",
                0.5,
            )
            .unwrap();
        assert_eq!(swing.buttons, FORWARD | FIRE);
        assert!(
            lunge
                .step(
                    30,
                    runner(200.0, 0.0, false),
                    target(300.0),
                    &weapons(),
                    "blade",
                    0.5
                )
                .is_none()
        );
        assert!(!lunge.active());
    }

    #[test]
    fn a_slide_that_never_starts_hops_aside_to_the_previous_gun() {
        let mut lunge = Lunge::default();
        let target = |_: &str| {
            Some(Mark {
                position: Vec3::new(400.0, 0.0, 0.0),
                velocity: Vec3::default(),
            })
        };
        lunge.start(0, "foe", "rlauncher", true);
        lunge.step(
            1,
            runner(0.0, 400.0, false),
            target,
            &weapons(),
            "rlauncher",
            0.5,
        );
        let hop = lunge
            .step(
                25,
                runner(0.0, 400.0, false),
                target,
                &weapons(),
                "blade",
                0.5,
            )
            .unwrap();
        assert_eq!(hop.buttons, JUMP | LEFT);
        assert_eq!(hop.weapon_slot, 2);
        // Facing +x, left of the run is +z.
        assert!(hop.direction[2] > 0.5, "{:?}", hop.direction);
        assert!(
            lunge
                .step(
                    37,
                    runner(0.0, 0.0, false),
                    target,
                    &weapons(),
                    "blade",
                    0.5
                )
                .is_none()
        );
        // A target lost mid-slide exits the same way.
        lunge.start(100, "foe", "rlauncher", false);
        let hop = lunge
            .step(
                101,
                runner(0.0, 400.0, false),
                |_| None,
                &weapons(),
                "rlauncher",
                0.5,
            )
            .unwrap();
        assert_eq!(hop.buttons, JUMP | RIGHT);
    }
}
