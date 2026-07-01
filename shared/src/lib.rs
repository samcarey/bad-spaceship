use bevy::{
    app::PluginGroupBuilder,
    ecs::component::Component,
    math::{Quat, Vec2, Vec3},
    prelude::{Bundle, Entity, Message, PluginGroup, Resource, SystemSet},
};
use character::CharacterPlugin;
use config::ConfigPlugin;
use map::MapPlugin;
use part::PartPlugin;
use player::PlayerPlugin;

pub mod character;
pub mod config;
pub mod map;
pub mod net;
pub mod part;
pub mod player;
pub mod utils;

pub struct CommonPlugins;

impl PluginGroup for CommonPlugins {
    fn build(self) -> PluginGroupBuilder {
        // NOTE: Avian's `PhysicsPlugins` is added separately by each binary via
        // `add_physics` (below), because in multiplayer two of its sub-plugins must
        // be disabled (handled by `lightyear_avian3d`) and that can't be expressed
        // through this group builder.
        PluginGroupBuilder::start::<Self>()
            // Custom plugins
            .add(CharacterPlugin)
            .add(ConfigPlugin)
            .add(MapPlugin)
            .add(PartPlugin)
            .add(PlayerPlugin)
    }
}

/// Add Avian's physics plugin group. In multiplayer, `lightyear_avian3d` takes over
/// the `Position`↔`Transform` sync and frame interpolation, so Avian's own
/// `PhysicsTransformPlugin` + `PhysicsInterpolationPlugin` must be disabled (doing
/// so in single-player would break rendering, which relies on Avian's sync). Call
/// once from each binary's `main`, before any `LightyearAvianPlugin`.
pub fn add_physics(app: &mut bevy::app::App, multiplayer: bool) {
    use avian3d::prelude::*;
    if multiplayer {
        app.add_plugins(
            PhysicsPlugins::default()
                .build()
                .disable::<PhysicsTransformPlugin>()
                .disable::<PhysicsInterpolationPlugin>(),
        );
    } else {
        app.add_plugins(PhysicsPlugins::default());
    }
}

#[derive(Default, Component)]
pub struct KeyboardDirectionalInput(pub Vec3);

#[derive(Default, Component)]
pub struct GameStickDirectionalInput(pub Vec3);

/// The player's look yaw (radians). Replicated so remote clients can face each
/// avatar the way it's looking — the body itself is `ROTATION_LOCKED` at identity
/// (the camera rig owns the look), so facing rides on this value, applied to the
/// rendered avatar's visual pivot (`face_replicated_players`).
#[derive(Default, Component, Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
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

/// When present, suppresses spawning the local single-player `Player`/`Character`.
/// Both netcode plugins insert it: the server simulates one `ServerAvatar` body per
/// connected client, and the client controls its *predicted* networked avatar
/// instead of a separate local character (so there's exactly one character on each).
#[derive(Default, Resource)]
pub struct SuppressLocalPlayer;

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

#[derive(Clone, Hash, Debug, PartialEq, Eq, Message)]
pub struct PlayerClick;

#[derive(Default, Component)]
pub struct Character;

#[derive(Default, Component)]
pub struct DirectionalInput(pub Vec3);

#[derive(SystemSet, Hash, Debug, PartialEq, Eq, Clone)]
pub struct InputEvents;

#[derive(Default, Component)]
pub struct PartRotation(pub Quat);

#[derive(Component)]
pub struct Focused;

#[derive(Component)]
pub struct Attachable;

#[derive(Default, Component)]
pub struct LeftClicked(pub bool);

#[derive(Default, Component)]
pub struct Modifying(pub bool);

#[derive(Message)]
struct AttachEvent;

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

/// TEMPORARY on-screen diagnostic for the delete-zone red-highlight bug (a joint
/// fully inside the sphere intermittently stays green in multiplayer). Populated by
/// `update_predelete_joints` from the *exact* same computation as the real detection,
/// so the numbers can't diverge from the logic. Shown in the HUD when `active`.
/// Remove once the root cause is found.
#[derive(Default, Resource)]
pub struct DeleteZoneDebug {
    /// The empty-handed + modifier branch ran this frame (delete mode is active).
    pub active: bool,
    /// Total `SphericalJoint` entities seen.
    pub joints: u32,
    /// Of those, how many had a resolvable `body2` `Holdable` transform.
    pub resolved: u32,
    /// How many were within `DELETE_RADIUS` (i.e. would highlight red).
    pub in_zone: u32,
    /// Nearest joint's `body2`-anchor distance to the hold point (what the detector
    /// actually tests), and its `body1`-anchor distance (where the MP gizmo is drawn).
    pub nearest_body2: f32,
    pub nearest_body1: f32,
}

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
pub struct LookPitch(pub f32);
