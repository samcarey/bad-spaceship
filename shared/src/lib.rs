use bevy::{
    math::{Quat, Vec2, Vec3},
    prelude::{Entity, KeyCode, MouseButton, SystemLabel},
};
use bevy_rapier3d::prelude::ColliderHandle;

pub mod character;
pub mod config;
pub mod contact;
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

#[derive(Default)]
pub struct KeyboardDirectionalInput(pub Vec3);

#[derive(Default)]
pub struct GameStickDirectionalInput(pub Vec3);

#[derive(Default)]
pub struct Yaw(pub f32);

#[derive(Default)]
pub struct CameraOrbitCenter;

#[derive(Default)]
pub struct FocusedInteractable(Option<Entity>);

#[derive(Default)]
pub struct Holding(pub bool);

#[derive(Default)]
struct Player;

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
    fn new(camera_entity: Entity) -> Self {
        OrbitingCamera {
            entity: Some(camera_entity),
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub struct HoldPoint;

#[derive(Default)]
pub struct MouseMotionDelta(pub Vec2);

#[derive(SystemLabel, Clone, Hash, Debug, PartialEq, Eq)]
pub struct PlayerClick;

#[derive(Default)]
pub struct Character;

#[derive(Default)]
struct DirectionalInput(Vec3);

#[derive(SystemLabel, Hash, Debug, PartialEq, Eq, Clone)]
pub struct InputEvents;

#[derive(Default)]
pub struct PartRotation(pub Quat);
#[derive(Debug, Hash, Ord, PartialOrd, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(feature = "serialize", derive(serde::Serialize, serde::Deserialize))]
pub struct WebKeyCode(pub KeyCode);

#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(feature = "serialize", derive(serde::Serialize, serde::Deserialize))]
pub struct WebMouseButton(pub MouseButton);

#[derive(Default)]
pub struct TouchingColliders(Vec<ColliderHandle>);

impl TouchingColliders {
    pub fn index(&self, handle: &ColliderHandle) -> Option<usize> {
        self.0.iter().position(|x| *x == *handle)
    }

    pub fn touching(&self) -> bool {
        !self.0.is_empty()
    }
}

pub struct Focused;

pub struct Attachable;
