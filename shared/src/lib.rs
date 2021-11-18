use bevy::{
    math::{Quat, Vec2, Vec3},
    prelude::{Bundle, Entity, KeyCode, MouseButton, PluginGroup, SystemLabel},
};
use bevy_easings::EasingsPlugin;
use bevy_rapier3d::{
    na::{Const, OPoint},
    physics::RapierPhysicsPlugin,
};
use character::CharacterPlugin;
use config::ConfigPlugin;
use map::MapPlugin;
use part::PartPlugin;
use player::PlayerPlugin;

pub mod character;
pub mod config;
pub mod map;
pub mod part;
pub mod player;
pub mod utils;

pub struct CommonPlugins;

impl PluginGroup for CommonPlugins {
    fn build(&mut self, group: &mut bevy::app::PluginGroupBuilder) {
        // Third-party plugins
        group
            .add(RapierPhysicsPlugin::<&IgnoreContactsWith>::default())
            .add(EasingsPlugin);

        // Custom plugins
        group
            .add(CharacterPlugin)
            .add(ConfigPlugin)
            .add(MapPlugin)
            .add(PartPlugin)
            .add(PlayerPlugin);
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

pub struct PlayerCameraOrbitCenter(pub Entity);

#[derive(Default)]
pub struct FocusedInteractable(pub Option<Entity>);

#[derive(Default)]
pub struct Holding(pub bool);

#[derive(Default)]
pub struct Player;

pub struct OrbitingCamera(pub Entity);

const INITIAL_CAMERA_PITCH_DEGREES: f32 = 30.;
pub const INITIAL_CAMERA_PITCH: f32 = INITIAL_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;

impl Default for LookPitch {
    fn default() -> Self {
        Self(INITIAL_CAMERA_PITCH)
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

pub struct Focused;

pub struct Attachable;

#[derive(Default)]
pub struct LeftClicked(pub bool);

#[derive(Default)]
pub struct Modifying(pub bool);

#[derive(Clone, Copy)]
struct IgnoreContactsWith(Entity);

struct AttachEvent;

struct ReleaseEvent;

struct HoldEvent {
    held: Entity,
}

type ContactPoint = OPoint<f32, Const<3>>;

pub struct DisplayableJoint {
    pub points: (ContactPoint, ContactPoint),
    pub entities: (Entity, Entity),
}

#[derive(Default)]
pub struct PotentialJoints(pub Vec<DisplayableJoint>);

#[derive(Default)]
pub struct ExistingJoints(pub Vec<DisplayableJoint>);

pub struct PredeleteJoint {
    pub entity: Entity,
    pub translation: Vec3,
}

#[derive(Default)]
pub struct PredeleteJoints(pub Vec<PredeleteJoint>);

#[derive(SystemLabel, Clone, Hash, Debug, PartialEq, Eq)]
struct ToggleHoldingSystemLabel;

#[derive(SystemLabel, Clone, Hash, Debug, PartialEq, Eq)]
pub struct UpdateJointsLabel;

struct BoundingRadius(f32);

#[derive(Default)]
pub struct OriginalPosition(Vec3);

pub struct Grass;

#[derive(Default)]
pub struct Click(bool);

#[derive(Default)]
pub struct MouseWheelDelta(pub f32);

#[derive(Bundle, Default)]
pub struct PlayerInput {
    directional_input: DirectionalInput,
    click: Click,
    modifying: Modifying,
    mouse_motion: MouseMotionDelta,
    mouse_wheel: MouseWheelDelta,
}

#[derive(SystemLabel, Clone, Hash, Debug, PartialEq, Eq)]
pub struct MouseWheelLabel;

pub struct LookPitch(f32);
