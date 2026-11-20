use serde::Deserialize;
use shagger_shared::net::game_state::Vec3;

use super::protocol::PlayerInput;

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
    routes: Vec<Route>,
}

#[derive(Deserialize)]
struct Route {
    entry: [f64; 3],
    waypoints: Vec<[f64; 3]>,
}

impl TryFrom<RouteFile> for WashoiNavigation {
    type Error = &'static str;

    fn try_from(file: RouteFile) -> Result<Self, Self::Error> {
        if file.map.trim().is_empty() || file.routes.is_empty() {
            return Err("navigation needs a map and at least one route");
        }
        for route in &file.routes {
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
            routes: file.routes,
        })
    }
}

impl WashoiNavigation {
    pub fn map(&self) -> &str {
        &self.map
    }

    pub(super) fn route_at(&self, position: Vec3) -> Option<usize> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, route)| (route.entry[1] - position.y).abs() <= MAX_HEIGHT_DELTA)
            .map(|(index, route)| (index, horizontal_distance(position, route.entry)))
            .filter(|(_, distance)| *distance <= ENTRY_RADIUS)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
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
    fn selects_a_nearby_route_on_the_same_floor_and_advances_waypoints() {
        let routes: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
                {"entry":[0,100,0],"waypoints":[[40,100,0]]},
                {"entry":[0,0,0],"waypoints":[[40,0,0],[40,0,40]]}
            ]}"#,
        )
        .unwrap();
        let route = routes.route_at(Vec3::new(0.0, 0.0, 0.0)).unwrap();
        assert_eq!(route, 1);
        assert_eq!(routes.route_at(Vec3::new(100.0, 0.0, 0.0)), None);
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
