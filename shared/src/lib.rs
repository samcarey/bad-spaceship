use bevy::{
    app::PluginGroupBuilder,
    ecs::component::Component,
    math::{Quat, Vec2, Vec3},
    prelude::{Bundle, Entity, KeyCode, MouseButton, PluginGroup, Resource, SystemSet},
};
use bevy_easings::EasingsPlugin;
use bevy_rapier3d::plugin::{NoUserData, RapierPhysicsPlugin};
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
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            // Third-party plugins
            .add(RapierPhysicsPlugin::<NoUserData>::default())
            .add(EasingsPlugin)
            // Custom plugins
            .add(CharacterPlugin)
            .add(ConfigPlugin)
            .add(MapPlugin)
            .add(PartPlugin)
            .add(PlayerPlugin)
    }
}

#[derive(Default, Component)]
pub struct KeyboardDirectionalInput(pub Vec3);

#[derive(Default, Component)]
pub struct GameStickDirectionalInput(pub Vec3);

#[derive(Default, Component)]
pub struct Yaw(pub f32);

#[derive(Default, Component)]
pub struct CameraOrbitCenter;

#[derive(Component)]
pub struct PlayerCameraOrbitCenter(pub Entity);

#[derive(Default, Component)]
pub struct FocusedInteractable(pub Option<Entity>);

#[derive(Default, Component)]
pub struct Holding(pub bool);

#[derive(Default, Component)]
pub struct Player;

#[derive(Component)]
pub struct OrbitingCamera(pub Entity);

const INITIAL_CAMERA_PITCH_DEGREES: f32 = 30.;
pub const INITIAL_CAMERA_PITCH: f32 = INITIAL_CAMERA_PITCH_DEGREES * utils::DEG_TO_RADIANS;

impl Default for LookPitch {
    fn default() -> Self {
        Self(INITIAL_CAMERA_PITCH)
    }
}

#[derive(Default, Component)]
pub struct HoldPoint;

#[derive(Default, Component)]
pub struct MouseMotionDelta(pub Vec2);

#[derive(Clone, Hash, Debug, PartialEq, Eq)]
pub struct PlayerClick;

#[derive(Default, Component)]
pub struct Character;

#[derive(Default, Component)]
struct DirectionalInput(Vec3);

#[derive(SystemSet, Hash, Debug, PartialEq, Eq, Clone)]
pub struct InputEvents;

#[derive(Default, Component)]
pub struct PartRotation(pub Quat);
#[derive(Debug, Hash, Ord, PartialOrd, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(feature = "serialize", derive(serde::Serialize, serde::Deserialize))]
pub struct WebKeyCode(pub KeyCode);

#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(feature = "serialize", derive(serde::Serialize, serde::Deserialize))]
pub struct WebMouseButton(pub MouseButton);

#[derive(Component)]
pub struct Focused;

#[derive(Component)]
pub struct Attachable;

#[derive(Default, Component)]
pub struct LeftClicked(pub bool);

#[derive(Default, Component)]
pub struct Modifying(pub bool);

struct AttachEvent;

struct ReleaseEvent;

struct HoldEvent {
    held: Entity,
}

pub struct DisplayableJoint {
    pub points: (Vec3, Vec3),
    pub entities: (Entity, Entity),
}

#[derive(Default, Resource)]
pub struct PotentialJoints(pub Vec<DisplayableJoint>);

#[derive(Default, Resource)]
pub struct ExistingJoints(pub Vec<DisplayableJoint>);

pub struct PredeleteJoint {
    pub entity: Entity,
    pub translation: Vec3,
}

#[derive(Default, Resource)]
pub struct PredeleteJoints(pub Vec<PredeleteJoint>);

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
struct ToggleHoldingSystemLabel;

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
pub struct UpdateJointsLabel;

#[derive(Component)]
struct BoundingRadius(f32);

#[derive(Default, Component)]
pub struct OriginalPosition(Vec3);

#[derive(Component)]
pub struct Grass;

#[derive(Default, Component)]
pub struct Click(bool);

#[derive(Default, Component)]
pub struct MouseWheelDelta(pub f32);

#[derive(Bundle, Default)]
pub struct PlayerInput {
    directional_input: DirectionalInput,
    click: Click,
    modifying: Modifying,
    mouse_motion: MouseMotionDelta,
    mouse_wheel: MouseWheelDelta,
}

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
pub struct MouseWheelLabel;

#[derive(Component)]
pub struct LookPitch(f32);
