#![cfg(feature = "washoi")]

use std::{sync::Arc, time::Duration};

use shagadelic::games::washoi::{
    BotPlayer, WashoiNavigation,
    protocol::{SnapshotEvent, WashoiEvents, connect},
};

#[tokio::test]
#[ignore = "requires a local standalone Washoi game server on washoi_dm2"]
async fn washoi_bot_reaches_an_authored_waypoint() {
    #[derive(serde::Deserialize)]
    struct Route {
        entry: [f64; 3],
        waypoints: Vec<[f64; 3]>,
    }
    #[derive(serde::Deserialize)]
    struct RouteFixture {
        routes: Vec<Route>,
    }

    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let fixture = include_str!("fixtures/washoi_dm2_navigation.json");
    let navigation: WashoiNavigation = serde_json::from_str(fixture).unwrap();
    let routes = serde_json::from_str::<RouteFixture>(fixture)
        .unwrap()
        .routes;
    let deadline = Duration::from_secs(8);
    let mut connection = connect(
        &endpoint,
        "SHAGADELIC navigation".into(),
        None,
        None,
        deadline,
        WashoiEvents,
        |_| {},
    )
    .await
    .expect("Washoi admission");
    assert_eq!(connection.welcome.as_ref().unwrap().map, navigation.map());
    let entity = connection.identity.entity_id.clone();
    let require_respawn = std::env::var_os("SHAGADELIC_TEST_WASHOI_RESPAWN").is_some();
    let mut saw_death = false;
    let mut saw_respawn = false;
    let mut start = None;
    let mut target = None;
    let mut reached = false;
    let mut last = None;
    let played = tokio::time::timeout(
        deadline,
        connection.play(
            BotPlayer::new(entity.clone()).with_navigation(Arc::new(navigation)),
            |state| {
                for event in &state.events {
                    if let SnapshotEvent::CombatState {
                        entity_id, dead, ..
                    } = event
                        && *entity_id == entity
                    {
                        saw_death |= *dead;
                        saw_respawn |= saw_death && !dead;
                    }
                }
                if require_respawn && !saw_respawn {
                    return true;
                }
                if let Some(position) = state
                    .entities
                    .get(&entity)
                    .and_then(|entity| entity.position)
                {
                    last = Some(position);
                    target.get_or_insert_with(|| {
                        start = Some(position);
                        let route = routes
                            .iter()
                            .min_by(|a, b| {
                                let distance = |p: [f64; 3]| {
                                    (p[0] - position.x)
                                        .hypot(p[2] - position.z)
                                        .hypot(p[1] - position.y)
                                };
                                distance(a.entry).total_cmp(&distance(b.entry))
                            })
                            .unwrap();
                        route.waypoints[0]
                    });
                    let [x, y, z] = target.unwrap();
                    reached = state.acked_input_seq >= 3
                        && (x - position.x).hypot(z - position.z) <= 16.0
                        && (y - position.y).abs() <= 40.0;
                }
                !reached
            },
        ),
    )
    .await;
    assert!(
        reached,
        "route from {start:?} to {target:?} ended at {last:?}: {played:?}"
    );
    played.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "requires a local standalone Washoi game server"]
async fn washoi_applies_paced_pilot_movement() {
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let ticket = std::env::var("SHAGADELIC_TEST_WASHOI_TICKET").ok();
    let deadline = Duration::from_secs(10);
    let mut connection = connect(
        &endpoint,
        "SHAGADELIC test".into(),
        ticket,
        None,
        deadline,
        WashoiEvents,
        |_| {},
    )
    .await
    .expect("both Washoi join stages");
    let entity = connection.identity.entity_id.clone();
    let mut start = None;
    let mut moved = false;
    tokio::time::timeout(
        deadline,
        connection.play(BotPlayer::new(entity.clone()), |state| {
            if let Some(position) = state
                .entities
                .get(&entity)
                .and_then(|entity| entity.position)
            {
                let start = *start.get_or_insert(position);
                let horizontal_delta = (position.x - start.x).hypot(position.z - start.z);
                moved = state.acked_input_seq >= 3 && horizontal_delta > 0.25;
            }
            !moved
        }),
    )
    .await
    .expect("movement deadline")
    .expect("connected while moving");
    assert!(
        moved,
        "server acknowledged inputs and moved the bot horizontally"
    );
}
