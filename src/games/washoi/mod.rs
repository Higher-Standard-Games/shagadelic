//! The Washoi integration. `agents/` holds the kinds of AI, `hosting/` how
//! they are hosted, and the top level what they share: the observation
//! contract, difficulty, random draws, and perception.

mod agents;
mod difficulty;
mod hosting;
mod observation;
mod perception;
mod random;
pub use agents::player::BotPlayer;
pub use agents::player::callouts::Locations;
pub use agents::player::navigation::WashoiNavigation;
pub use agents::player::navigation::learned::LearnedRoutes;
pub use agents::player::strategy::intel::{Baseline, PeopleProfile};
pub use difficulty::Difficulty;
pub use hosting::net::protocol;
pub use hosting::net::session::{BotReport, SpawnBot, WashoiSession};
pub use observation::{Body, Controls, Observation, PlayerView, SimTime, Vec3, Velocity};
pub use perception::sight::Sight;
