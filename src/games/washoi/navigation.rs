mod follower;
mod graph;

use serde::Deserialize;
use shagger_shared::net::game_state::Vec3;

use super::{goals::PickupGoal, protocol::PlayerInput};

pub(super) use follower::{RouteFollower, RouteObservation};

const ENTRY_RADIUS: f64 = 48.0;
const ARRIVAL_RADIUS: f64 = 16.0;
const MAX_HEIGHT_DELTA: f64 = 40.0;

/// Authored walking routes. Coordinates are Washoi world-space XYZ.
#[derive(Deserialize)]
#[serde(try_from = "RouteFile")]
pub struct WashoiNavigation {
    map: String,
    routes: Vec<Route>,
}

#[derive(Deserialize)]
struct RouteFile {
    map: String,
    #[serde(default)]
    routes: Vec<Route>,
    graph: Option<graph::WalkGraph>,
}

#[derive(Deserialize)]
struct Route {
    entry: [f64; 3],
    waypoints: Vec<[f64; 3]>,
    learned_route: Option<String>,
    #[serde(default)]
    resumed_from: Option<String>,
    #[serde(default)]
    pickup: Option<PickupGoal>,
}

impl TryFrom<RouteFile> for WashoiNavigation {
    type Error = &'static str;

    fn try_from(file: RouteFile) -> Result<Self, Self::Error> {
        let routes = match file.graph {
            Some(graph) if file.routes.is_empty() => graph.routes()?,
            Some(_) => return Err("configure authored routes or a graph"),
            None => file.routes,
        };
        if file.map.trim().is_empty() || routes.is_empty() {
            return Err("navigation needs a map and at least one route");
        }
        for route in &routes {
            if route.waypoints.is_empty()
                || !std::iter::once(&route.entry)
                    .chain(&route.waypoints)
                    .flatten()
                    .all(|value| value.is_finite())
            {
                return Err("navigation routes need finite coordinates and at least one waypoint");
            }
        }
        Ok(Self {
            map: file.map,
            routes,
        })
    }
}

impl WashoiNavigation {
    pub fn map(&self) -> &str {
        &self.map
    }

    pub(super) fn has_learned_links(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.learned_route.is_some())
    }

    pub fn validate_learned_links(
        &self,
        library: Option<&super::LearnedRoutes>,
    ) -> Result<(), &'static str> {
        for route in &self.routes {
            if let Some(id) = &route.learned_route
                && !library.is_some_and(|library| {
                    let [x, y, z] = *route.waypoints.last().expect("validated route");
                    library.matches_takeoff(id, Vec3::new(x, y, z))
                })
            {
                return Err("learned route link must end near a known takeoff");
            }
            if let Some(id) = &route.resumed_from
                && !library.is_some_and(|library| {
                    let [x, y, z] = route.entry;
                    library.matches_landing(id, Vec3::new(x, y, z))
                })
            {
                return Err("learned continuation must start near a known landing");
            }
        }
        Ok(())
    }

    fn route_at(&self, position: Vec3, allowed: impl Fn(&Route) -> bool) -> Option<usize> {
        self.select_route(position, allowed, |_| 0.0)
    }

    fn select_route(
        &self,
        position: Vec3,
        allowed: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
    ) -> Option<usize> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, route)| allowed(route))
            .filter(|(_, route)| (route.entry[1] - position.y).abs() <= MAX_HEIGHT_DELTA)
            .filter_map(|(index, route)| {
                let distance = horizontal_distance(position, route.entry);
                (distance <= ENTRY_RADIUS).then(|| (index, distance, score(route)))
            })
            .min_by(
                |(a_index, a_distance, a_score), (b_index, b_distance, b_score)| {
                    a_distance
                        .total_cmp(b_distance)
                        .then_with(|| {
                            self.routes[*b_index]
                                .learned_route
                                .is_some()
                                .cmp(&self.routes[*a_index].learned_route.is_some())
                        })
                        .then_with(|| b_score.total_cmp(a_score))
                },
            )
            .map(|(index, _, _)| index)
    }

    pub(super) fn steer(&self, route: usize, next: &mut usize, position: Vec3) -> PlayerInput {
        let waypoints = &self.routes[route].waypoints;
        while let Some(&waypoint) = waypoints.get(*next) {
            if (waypoint[1] - position.y).abs() > MAX_HEIGHT_DELTA {
                break;
            }
            let distance = horizontal_distance(position, waypoint);
            if distance <= ARRIVAL_RADIUS {
                *next += 1;
                continue;
            }
            let x = (waypoint[0] - position.x) / distance;
            let z = (waypoint[2] - position.z) / distance;
            return PlayerInput {
                direction: [x, 0.0, z],
                yaw: x.atan2(z),
                ..PlayerInput::default()
            };
        }
        PlayerInput::default()
    }
}

fn horizontal_distance(position: Vec3, target: [f64; 3]) -> f64 {
    (target[0] - position.x).hypot(target[2] - position.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_graph_uses_the_detour_instead_of_the_blocked_shortcut() {
        let baked = include_str!("../../../tests/fixtures/washoi_dm2_walk_graph.json");
        let routes: WashoiNavigation = serde_json::from_str(baked).unwrap();
        let route = routes
            .route_at(Vec3::new(-184.0, 192.0, 288.0), |_| true)
            .unwrap();
        assert_eq!(
            routes.routes[route].waypoints,
            [[-232.0, 192.0, 192.0], [-88.0, 192.0, 312.0],]
        );
        let config: serde_json::Value = serde_json::from_str(baked).unwrap();
        let mut invalid = config.clone();
        invalid["graph"]["edges"][0] = serde_json::json!([0, 999]);
        assert!(serde_json::from_value::<WashoiNavigation>(invalid).is_err());
        let mut disconnected = config;
        disconnected["graph"]["edges"] = serde_json::json!([]);
        assert!(serde_json::from_value::<WashoiNavigation>(disconnected).is_err());
    }

    #[test]
    fn selects_a_nearby_route_on_the_same_floor_and_advances_waypoints() {
        let routes: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
                {"entry":[0,100,0],"waypoints":[[40,100,0]]},
                {"entry":[0,0,0],"waypoints":[[40,0,0],[40,0,40]]}
            ]}"#,
        )
        .unwrap();
        let route = routes.route_at(Vec3::new(0.0, 0.0, 0.0), |_| true).unwrap();
        assert_eq!(route, 1);
        assert_eq!(routes.route_at(Vec3::new(100.0, 0.0, 0.0), |_| true), None);
        let mut next = 0;
        assert_eq!(
            routes
                .steer(route, &mut next, Vec3::new(0.0, 0.0, 0.0))
                .direction,
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            routes
                .steer(route, &mut next, Vec3::new(40.0, 0.0, 0.0))
                .direction,
            [0.0, 0.0, 1.0]
        );
        assert_eq!(
            routes
                .steer(route, &mut next, Vec3::new(40.0, 0.0, 40.0))
                .direction,
            [0.0; 3]
        );
        assert_eq!(next, 2);
    }
}
