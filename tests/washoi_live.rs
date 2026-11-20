#![cfg(feature = "washoi")]

use std::time::Duration;

use shagadelic::games::washoi::{
    BotPlayer,
    protocol::{WashoiEvents, connect},
};

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
