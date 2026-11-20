mod behavior;
mod navigation;
pub mod protocol;
mod session;
pub use behavior::BotPlayer;
pub use navigation::WashoiNavigation;
pub use session::{BotReport, SpawnBot, WashoiSession};
