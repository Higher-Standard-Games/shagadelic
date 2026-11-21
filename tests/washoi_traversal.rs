#![cfg(feature = "washoi")]

use std::{sync::Arc, time::Duration};

use shagadelic::games::washoi::{
    BotPlayer, WashoiNavigation,
    protocol::{WashoiEvents, connect},
};

#[tokio::test]
#[ignore = "requires the local washoi_dm2 traversal fixture"]
async fn washoi_confirms_a_graph_traversal() {
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").unwrap();
    let navigation =
        WashoiNavigation::load(std::env::var_os("SHAGADELIC_TEST_TRAVERSAL_GRAPH").unwrap())
            .unwrap();
    let kind = std::env::var("SHAGADELIC_TEST_TRAVERSAL_KIND").unwrap();
    let target: [f64; 3] =
        serde_json::from_str(&std::env::var("SHAGADELIC_TEST_TRAVERSAL_TARGET").unwrap()).unwrap();
    let limit = Duration::from_secs(7);
    let mut connection = connect(
        &endpoint,
        "Traversal fixture".into(),
        None,
        None,
        limit,
        WashoiEvents,
        |_| {},
    )
    .await
    .unwrap();
    let id = connection.identity.entity_id.clone();
    let bot = BotPlayer::new(id.clone()).with_navigation(Arc::new(navigation));
    let mut previous = None;
    let mut airborne = false;
    let mut went_through = false;
    let mut highest = f64::NEG_INFINITY;
    let mut reached = false;
    let result = tokio::time::timeout(
        limit,
        connection.play(bot, |state| {
            if let Some(entity) = state.entities.get(&id)
                && let (Some(p), Some(v)) = (entity.position, entity.velocity)
            {
                highest = highest.max(p.y);
                airborne |= !v.grounded && v.linear.y > 80.0;
                if let Some([x, y, z]) = previous.replace([p.x, p.y, p.z]) {
                    went_through |= (p.x - x).hypot(p.y - y).hypot(p.z - z) > 150.0;
                }
                reached = state.acked_input_seq > 0
                    && (p.x - target[0]).hypot(p.z - target[2]) <= 72.0
                    && (p.y - target[1]).abs() <= 72.0
                    && if kind == "teleporter" {
                        went_through
                    } else {
                        airborne && v.grounded
                    };
            }
            !reached
        }),
    )
    .await;
    assert!(
        reached,
        "{kind} missed {target:?}; last {previous:?}, apex {highest}, airborne {airborne}, result {result:?}"
    );
    result.unwrap().unwrap();
}
