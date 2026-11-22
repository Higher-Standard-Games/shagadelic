//! The Washoi integration. `agents/` holds the kinds of AI, `hosting/` how
//! they are hosted, and the top level what they share: the observation
//! contract, difficulty, random draws, and perception.

mod agents;
mod difficulty;
mod hosting;
mod observation;
mod perception;
mod random;
pub use agents::player::callouts::Locations;
pub use agents::player::navigation::WashoiNavigation;
pub use agents::player::navigation::learned::LearnedRoutes;
pub use agents::player::strategy::intel::{Baseline, PeopleProfile};
pub use agents::player::{BotPlayer, HunchMode};
pub use difficulty::Difficulty;
pub use hosting::net::protocol;
pub use hosting::net::session::{BotReport, SpawnBot, WashoiSession};
pub use observation::{Body, Controls, Observation, PlayerView, SimTime, Vec3, Velocity};
pub use perception::collision::MapCollision;
pub use perception::sight::Sight;

/// Whether a player name is one of the bot names, as SHAG's `isBotName`
/// decides: tools use it to find the human seats in a recording.
pub fn is_bot_name(name: &str) -> bool {
    agents::player::chat::is_bot_name(name)
}

#[cfg(test)]
mod movement_tests {
    use shagger_shared::network::components::input_component::input_buttons;
    use shagger_shared::network::systems::movement::movement_profile::MovementInput;
    use shagger_shared::server::simulation::types::Vec3 as SimVec3;
    use washoi_movement::simulation::movement_profile::MovementProfile;

    /// Washoi's own movement simulation runs here with the same
    /// `shagger_shared` types the bot uses. With no collision world the body
    /// counts as grounded, and forward with sprint runs it up to the 400 u/s
    /// sprint speed and no further.
    #[test]
    fn steps_washoi_movement_from_the_bot_crate() {
        let mut profile = MovementProfile::default();
        let input = MovementInput {
            direction: SimVec3 {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            buttons: input_buttons::FORWARD | input_buttons::SPRINT,
            ..MovementInput::default()
        };
        let start = SimVec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let mut position = start;
        let mut velocity = SimVec3 {
            x: 0.0,
            y: 0.0,
            z: 300.0,
        };
        for _ in 0..10 {
            let step = profile.simulate(position, velocity, Some(&input), 0.008, None);
            position = step.position;
            velocity = step.velocity;
        }
        assert!(
            velocity.z > 300.0 && velocity.z <= 400.0,
            "sprints: {velocity:?}"
        );
        assert!(position.z > start.z, "keeps its run: {position:?}");
    }
}
