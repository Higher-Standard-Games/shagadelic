//! Gates on the trigger, from TypeScript `bot/BotFireSafety.ts`, applied to
//! every command as `BotClient._aimed` does.
//!
//! Fire waits until the view sent is on the aim: the brain presses fire as
//! soon as it decides to shoot, but the view takes a reaction time and a
//! turn to get there, and a player does not fire all the way round. Alt-fire
//! (sights, the blade's charge) is left alone, as a player raises the sights
//! while still turning. Then no attack goes out while a living ally stands
//! on the line of the view sent.

use crate::games::washoi::{
    Vec3,
    agents::player::combat::{
        ALT_FIRE, FIRE,
        aim::{View, wrap},
    },
};

const DEG: f64 = std::f64::consts::PI / 180.0;
/// `FIRE_START_CONE`: a shot starts only this close to the aim.
const FIRE_START_CONE: f64 = 12.0 * DEG;
/// `FIRE_KEEP_CONE`: once shooting, the trigger stays down until this far off.
const FIRE_KEEP_CONE: f64 = 25.0 * DEG;
/// `applyFriendlyFireSafety` defaults: eye height, ally radius, and range.
const EYE_HEIGHT: f64 = 32.0;
const ALLY_RADIUS: f64 = 36.0;
const MAX_DISTANCE: f64 = 4000.0;

/// `holdFireOffAim`: the buttons to send, and whether the bot is now shooting.
pub(in crate::games::washoi) fn hold_fire_off_aim(
    buttons: u16,
    aim: View,
    sent: View,
    shooting: bool,
) -> (u16, bool) {
    if buttons & FIRE == 0 {
        return (buttons, false);
    }
    let off = wrap(aim.yaw - sent.yaw)
        .abs()
        .max((aim.pitch - sent.pitch).abs());
    let cone = if shooting {
        FIRE_KEEP_CONE
    } else {
        FIRE_START_CONE
    };
    if off <= cone {
        (buttons, true)
    } else {
        (buttons & !FIRE, false)
    }
}

/// `applyFriendlyFireSafety`: the buttons without fire and alt-fire when an
/// ally is within 36 units of the line of sight ahead, up to 4,000 units.
pub(in crate::games::washoi) fn spare_allies(
    buttons: u16,
    position: Vec3,
    view: View,
    allies: &[Vec3],
) -> u16 {
    let attack = FIRE | ALT_FIRE;
    if buttons & attack == 0 || allies.is_empty() {
        return buttons;
    }
    let origin = [position.x, position.y + EYE_HEIGHT, position.z];
    let forward = [
        view.yaw.sin() * view.pitch.cos(),
        -view.pitch.sin(),
        view.yaw.cos() * view.pitch.cos(),
    ];
    let blocked = allies.iter().any(|ally| {
        let to = [
            ally.x - origin[0],
            ally.y + EYE_HEIGHT * 0.75 - origin[1],
            ally.z - origin[2],
        ];
        let along = to[0] * forward[0] + to[1] * forward[1] + to[2] * forward[2];
        let off = to[0] * to[0] + to[1] * to[1] + to[2] * to[2] - along * along;
        along > 0.0 && along <= MAX_DISTANCE && off <= ALLY_RADIUS * ALLY_RADIUS
    });
    if blocked { buttons & !attack } else { buttons }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(yaw: f64) -> View {
        View { yaw, pitch: 0.0 }
    }

    #[test]
    fn fires_only_once_the_view_is_on_the_aim_and_keeps_firing_through_small_drift() {
        let aim = view(0.0);
        // Still turning, 30 degrees off: no shot yet; alt-fire passes.
        assert_eq!(
            hold_fire_off_aim(FIRE | ALT_FIRE, aim, view(30.0 * DEG), false),
            (ALT_FIRE, false)
        );
        // On the aim within 12 degrees: fire.
        assert_eq!(
            hold_fire_off_aim(FIRE, aim, view(10.0 * DEG), false),
            (FIRE, true)
        );
        // Shooting, 20 degrees off still fires; 30 does not.
        assert_eq!(
            hold_fire_off_aim(FIRE, aim, view(20.0 * DEG), true),
            (FIRE, true)
        );
        assert_eq!(
            hold_fire_off_aim(FIRE, aim, view(30.0 * DEG), true),
            (0, false)
        );
        assert_eq!(hold_fire_off_aim(0, aim, view(0.0), true), (0, false));
    }

    #[test]
    fn holds_attacks_with_an_ally_on_the_line() {
        let me = Vec3::new(0.0, 0.0, 0.0);
        // Facing +z with an ally 300 units ahead, slightly to the side.
        let ally = [Vec3::new(20.0, 0.0, 300.0)];
        assert_eq!(spare_allies(FIRE | ALT_FIRE, me, view(0.0), &ally), 0);
        // Facing away, or the ally well off the line: the shot goes.
        assert_eq!(
            spare_allies(FIRE, me, view(std::f64::consts::PI), &ally),
            FIRE
        );
        assert_eq!(spare_allies(FIRE, me, view(0.5), &ally), FIRE);
        assert_eq!(spare_allies(FIRE, me, view(0.0), &[]), FIRE);
    }
}
