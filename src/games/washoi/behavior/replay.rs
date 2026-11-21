//! Regression trace captured against the pre-separation brain at `a272bda`.

use std::{sync::Arc, time::Duration};

use serde::Deserialize;

use crate::games::washoi::{
    BotPlayer, LearnedRoutes, Observation, SimTime, Vec3, Velocity, protocol::SnapshotEvent,
};

#[derive(Deserialize)]
struct Replay {
    entity_id: String,
    learned_routes: LearnedRoutes,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    ms: u64,
    snapshot_tick: Option<u32>,
    #[serde(default)]
    death: bool,
    #[serde(default)]
    respawn: bool,
    #[serde(default)]
    inputs: Vec<ExpectedInput>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ExpectedInput([f64; 3], f64, f64, u16, u8);

#[test]
fn replays_exact_inputs_and_remembers_a_one_tick_death() {
    let trace: Replay = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/washoi_decision_replay.json"
    ))
    .unwrap();
    let mut bot =
        BotPlayer::new(trace.entity_id.clone()).with_learned_routes(Arc::new(trace.learned_routes));
    let start = SimTime::ZERO;

    for step in trace.steps {
        let now = start + Duration::from_millis(step.ms);
        if let Some(tick) = step.snapshot_tick {
            let mut world = Observation {
                tick,
                ..Observation::default()
            };
            let own = world.body_or_insert(&trace.entity_id);
            own.position = Some(Vec3::default());
            own.velocity = Some(Velocity {
                grounded: true,
                ..Velocity::default()
            });
            if step.death {
                world.events.push(SnapshotEvent::CombatState {
                    entity_id: trace.entity_id.clone(),
                    health: 0,
                    armor: 0,
                    dead: true,
                });
            }
            bot.observe_at(&world, now);
        }
        assert_eq!(
            bot.request_respawn_at(now),
            step.respawn,
            "at {} ms",
            step.ms
        );
        let inputs: Vec<_> = bot
            .inputs_at(now)
            .map(|i| ExpectedInput(i.direction, i.yaw, i.pitch, i.buttons, i.weapon_slot))
            .collect();
        assert_eq!(inputs, step.inputs, "at {} ms", step.ms);
    }
}
