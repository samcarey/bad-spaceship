use bevy::{
    math::{Vec2, Vec3},
    prelude::{Bundle, Color, Entity},
};

pub mod character;
pub mod config;
pub mod map;
pub mod part;
pub mod player;
pub mod utils;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

#[derive(Clone, Copy)]
pub struct PlayerToSpawn {
    pub camera: Entity,
    pub size: f32,
    pub character: Entity,
}

#[derive(Default)]
pub struct KeyboardDirectionalInput(pub Vec3);

#[derive(Default)]
pub struct GameStickDirectionalInput(pub Vec3);

#[derive(Default)]
pub struct Yaw(pub f32);

#[derive(Default)]
pub struct CameraOrbitCenter;

#[derive(Default)]
pub struct FocusedInteractable {
    pub current: Option<Entity>,
    pub previous: Option<Entity>,
    pub previous_color: Option<Color>,
}

#[derive(Default)]
pub struct Holding(pub bool);

#[derive(Default, Bundle)]
pub struct Player {
    pub camera: OrbitingCamera,
}

pub struct OrbitingCamera {
    pub pitch: f32,
    pub entity: Option<Entity>,
}

const INITIAL_CAMERA_PITCH_DEGREES: f32 = 30.;
pub const INITIAL_CAMERA_PITCH: f32 = INITIAL_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;

impl Default for OrbitingCamera {
    fn default() -> Self {
        OrbitingCamera {
            pitch: INITIAL_CAMERA_PITCH,
            entity: None,
        }
    }
}

impl OrbitingCamera {
    fn new(camera_entity: Option<Entity>) -> Self {
        OrbitingCamera {
            entity: camera_entity,
            ..Default::default()
        }
    }
}

impl Player {
    pub fn new(camera_entity: Option<Entity>) -> Self {
        Player {
            camera: OrbitingCamera::new(camera_entity),
        }
    }
}

#[derive(Default)]
pub struct HoldPoint;

#[derive(Default)]
pub struct MouseMotionDelta(pub Vec2);

pub struct PlayerClick;

pub struct Character;
