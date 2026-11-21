#![cfg(feature = "washoi")]

use std::{sync::Arc, time::Duration};

use shagadelic::games::washoi::{
    BotPlayer, WashoiNavigation,
    protocol::{SnapshotEvent, WashoiEvents, connect},
};

#[tokio::test]
#[ignore = "requires a local Washoi server issuing the dm2 hold-area fixture"]
async fn washoi_follows_a_server_order_and_holds_the_destination() {
    let navigation: WashoiNavigation = serde_json::from_str(
        r#"{"map":"washoi_dm2","graph":{
            "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "nodes":[[-184,192,288],[-232,192,192],[-88,192,312]],
            "edges":[[0,1],[1,2]],"entries":[0],"goals":[1]
        }}"#,
    )
    .unwrap();
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let deadline = Duration::from_secs(8);
    let mut connection = connect(
        &endpoint,
        "SHAGADELIC objective".into(),
        None,
        None,
        deadline,
        WashoiEvents,
        |_| {},
    )
    .await
    .expect("Washoi admission");
    let entity_id = connection.identity.entity_id.clone();
    let mut got_directive = false;
    let mut passed_corner = false;
    let mut held_since = None;
    let mut held = false;
    let mut last = None;
    let result = tokio::time::timeout(
        deadline,
        connection.play(
            BotPlayer::new(entity_id.clone()).with_navigation(Arc::new(navigation)),
            |state| {
                got_directive |= state.events.iter().any(|event| {
                    matches!(event, SnapshotEvent::AgentDirectives { entity_id: id, .. } if *id == entity_id)
                });
                if let Some(position) = state.entities.get(&entity_id).and_then(|e| e.position) {
                    last = Some(position);
                    let at = |x: f64, z: f64, radius: f64| {
                        (position.x - x).hypot(position.z - z) <= radius
                            && (position.y - 192.0).abs() <= 40.0
                    };
                    passed_corner |= got_directive && at(-232.0, 192.0, 48.0);
                    if passed_corner && at(-88.0, 312.0, 32.0) {
                        let since = *held_since.get_or_insert(state.tick);
                        held = state.tick.saturating_sub(since) >= 40;
                    } else {
                        held_since = None;
                    }
                }
                !held
            },
        ),
    )
    .await;
    assert!(
        held,
        "directive received: {got_directive}, corner reached: {passed_corner}, last position: {last:?}, connection result: {result:?}"
    );
    result.unwrap().unwrap();
}
