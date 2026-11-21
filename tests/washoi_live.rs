#![cfg(feature = "washoi")]

use std::{sync::Arc, time::Duration};

use shagadelic::games::washoi::{
    BotPlayer, LearnedRoutes, WashoiNavigation,
    protocol::{SnapshotEvent, WashoiEvents, connect},
};

#[tokio::test]
#[ignore = "requires the local washoi_dm2 health-pickup fixture"]
async fn washoi_bot_collects_health_from_a_pickup_goal() {
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let navigation: WashoiNavigation = serde_json::from_value(serde_json::json!({
        "map":"washoi_dm2",
        "routes":[{
            "entry":[268,169.03125,112],"waypoints":[[284,171.03125,36]],
            "pickup":{"id":5,"kind":"health"}
        }]
    }))
    .unwrap();
    let deadline = Duration::from_secs(8);
    let mut connection = connect(
        &endpoint,
        "SHAGADELIC pickup".into(),
        None,
        None,
        deadline,
        WashoiEvents,
        |_| {},
    )
    .await
    .unwrap();
    let entity = connection.identity.entity_id.clone();
    let mut damage = None;
    let mut recovered = false;
    let result = tokio::time::timeout(
        deadline,
        connection.play(
            BotPlayer::new(entity.clone()).with_navigation(Arc::new(navigation)),
            |state| {
                for event in &state.events {
                    if let SnapshotEvent::CombatState {
                        entity_id,
                        health,
                        dead: false,
                        ..
                    } = event
                        && *entity_id == entity
                    {
                        if let Some(low) = damage {
                            recovered |=
                                *health > low && state.pickup_active_mask.get(5) == Some(&false);
                        }
                        if *health < 100 {
                            damage = Some(damage.map_or(*health, |low: u16| low.min(*health)));
                        }
                    }
                }
                !recovered
            },
        ),
    )
    .await;
    assert!(
        recovered,
        "health pickup was not confirmed after damage {damage:?}: {result:?}"
    );
}

#[tokio::test]
#[ignore = "requires a Washoi fixture that spawns the bot at the first learned route"]
async fn washoi_replays_the_existing_learned_route() {
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let path =
        std::env::var("SHAGADELIC_TEST_WASHOI_LEARNED").expect("original learned routes file");
    let library = Arc::new(LearnedRoutes::load(&path).unwrap());
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let route = &raw["routes"][0];
    let start = &route["start"];
    let [start_x, start_y, start_z] = ["x", "y", "z"].map(|axis| start[axis].as_f64().unwrap());
    let approach = std::env::var_os("SHAGADELIC_TEST_WASHOI_APPROACH").is_some();
    let require_continuation = std::env::var_os("SHAGADELIC_TEST_WASHOI_CONTINUE").is_some();
    let landing = &route["landing"];
    let coordinate = |axis| landing[axis].as_f64().unwrap();
    let [x, y, z] = [coordinate("x"), coordinate("y"), coordinate("z")];
    let ticks = route["ticks"].as_u64().unwrap();
    let deadline = Duration::from_secs(8);
    let mut connection = connect(
        &endpoint,
        "SHAGADELIC learned".into(),
        None,
        None,
        deadline,
        WashoiEvents,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(connection.welcome.as_ref().unwrap().map, library.map());
    let entity = connection.identity.entity_id.clone();
    let mut bot = BotPlayer::new(entity.clone()).with_learned_routes(library.clone());
    if approach {
        let authored = serde_json::json!({
            "map":library.map(),
            "routes":[{
                "entry":[start_x,start_y,start_z + 48.0],
                "waypoints":[[start_x,start_y,start_z]],
                "learned_route":route["id"],
            }],
        });
        let navigation: WashoiNavigation =
            match std::env::var_os("SHAGADELIC_TEST_WASHOI_WALK_GRAPH") {
                Some(path) => {
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
                }
                None => serde_json::from_value(authored).unwrap(),
            };
        navigation.validate_learned_links(Some(&library)).unwrap();
        bot = bot.with_navigation(Arc::new(navigation));
    }
    let mut landed = false;
    let mut continued = false;
    let mut last = None;
    let mut saw_offset_spawn = false;
    let result = tokio::time::timeout(
        deadline,
        connection.play(bot, |state| {
            if let Some(entity) = state.entities.get(&entity)
                && let (Some(position), Some(velocity)) = (entity.position, entity.velocity)
            {
                if last.is_none() {
                    saw_offset_spawn = (position.x - start_x).hypot(position.z - start_z) > 32.0;
                }
                last = Some((position, state.acked_input_seq));
                landed |= u64::from(state.acked_input_seq) + 1 >= ticks
                    && velocity.grounded
                    && (position.x - x).hypot(position.z - z) <= 64.0
                    && (position.y - y).abs() <= 48.0;
                continued |= landed
                    && velocity.grounded
                    && (position.x - (x - 72.0)).hypot(position.z - z) <= 10.0
                    && (position.y - y).abs() <= 16.0;
            }
            !(landed && (!require_continuation || continued))
        }),
    )
    .await;
    assert!(
        landed,
        "no learned landing near {:?}, last {last:?}: {result:?}",
        [x, y, z]
    );
    assert!(
        !require_continuation || continued,
        "no walking continuation reached after landing, last {last:?}: {result:?}"
    );
    assert!(
        !approach || saw_offset_spawn,
        "fixture did not start the bot away from takeoff"
    );
}

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
    let baked = std::env::var_os("SHAGADELIC_TEST_WASHOI_WALK_GRAPH")
        .map(|path| std::fs::read_to_string(path).expect("baked navigation graph"));
    let navigation: WashoiNavigation =
        serde_json::from_str(baked.as_deref().unwrap_or(fixture)).unwrap();
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
                        *route.waypoints.last().unwrap()
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
