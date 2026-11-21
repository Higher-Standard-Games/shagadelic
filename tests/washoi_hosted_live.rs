#![cfg(feature = "washoi")]

//! Live checks of the hosted path: the shared runtime owns the brains and the
//! socket tasks only do I/O. `washoi_live.rs` covers the standalone pilot.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use shagadelic::{
    games::washoi::{
        BotReport, Difficulty, Sight, SpawnBot, WashoiNavigation, WashoiSession,
        protocol::{SnapshotEvent, WashoiEvents, connect},
    },
    runtime::{Budget, Runtime},
};

#[tokio::test]
#[ignore = "requires a local Washoi server, a baked cache graph, and a sight file"]
async fn hosted_bots_fight_until_one_kills_the_other() {
    let endpoint = std::env::var("SHAGADELIC_TEST_WASHOI").expect("local Washoi endpoint");
    let graph = std::env::var("SHAGADELIC_TEST_WASHOI_ROAM_GRAPH").expect("baked cache graph");
    let sight = std::env::var("SHAGADELIC_TEST_WASHOI_SIGHT").expect("baked sight file");
    let navigation = WashoiNavigation::load(graph).unwrap();
    let sight = Sight::load(sight).unwrap();

    // A spectator watches the match. Perception skips spectators, so the
    // fighters only see each other.
    let mut watcher = connect(
        &endpoint,
        "SHAGADELIC hosted watcher".into(),
        None,
        Some("spectator"),
        Duration::from_secs(10),
        WashoiEvents,
        |_| {},
    )
    .await
    .unwrap();
    let watcher_entity = watcher.identity.entity_id.clone();
    let killed = Arc::new(AtomicBool::new(false));
    let watch = tokio::spawn({
        let killed = killed.clone();
        async move {
            while let Ok(state) = watcher.next_snapshot().await {
                let kill = state.events.iter().any(|event| {
                    matches!(event, SnapshotEvent::Death { entity_id, killer_id, .. }
                        if entity_id != killer_id
                            && *entity_id != watcher_entity
                            && *killer_id != watcher_entity)
                });
                if kill {
                    killed.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    });

    let mut runtime = Runtime::new(
        WashoiSession::new(endpoint)
            .with_navigation(navigation)
            .with_sight(sight),
    );
    for id in ["one", "two"] {
        runtime
            .spawn(
                id.into(),
                SpawnBot {
                    name: format!("SHAGADELIC hosted gunner {id}"),
                    ticket: None,
                    team: None,
                    difficulty: Difficulty::Insane,
                },
            )
            .unwrap();
    }

    let start = tokio::time::Instant::now();
    let budget = Budget {
        events: 64,
        inputs: 8,
        decisions: 8,
    };
    let mut admitted = 0;
    while !killed.load(Ordering::Relaxed) && start.elapsed() < Duration::from_secs(120) {
        runtime.advance(start.elapsed(), budget).unwrap();
        while let Some(report) = runtime.session_mut().take_report() {
            match report {
                BotReport::Admitted { .. } => admitted += 1,
                BotReport::Ended { key, reason } => panic!("{} ended: {reason}", key.id),
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    runtime.shutdown();
    watch.abort();
    assert_eq!(admitted, 2);
    assert!(
        killed.load(Ordering::Relaxed),
        "no hosted bot killed the other within 120 s"
    );
}
