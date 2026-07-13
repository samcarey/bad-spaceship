use std::f32;

use bevy::{
    core_pipeline::tonemapping::Tonemapping,
    math::Vec3A,
    prelude::*,
    reflect::TypePath,
};
use avian3d::prelude::Collider;
use serde::Deserialize;

use crate::{
    part::{Holdable, TargetOrientation, TargetPosition},
    utils::{ToVec3, DEG_TO_RADIANS},
    AttachEvent, BoundingRadius, CameraOrbitCenter, Character, FocusedInteractable,
    GameStickDirectionalInput, HoldPoint, Holding, InputEvents,
    KeyboardDirectionalInput, LeftClicked, LookPitch, Modifying, MouseMotionDelta, MouseWheelDelta,
    MouseWheelLabel, OriginalPosition, PartRotation, Player, PlayerCameraOrbitCenter, PlayerClick,
    PlayerHoldPoint, PlayerInput, ToggleHoldingSystemLabel, Yaw, INITIAL_CAMERA_PITCH,
};

const MAX_CAMERA_PITCH_DEGREES: f32 = 89.;
const MIN_CAMERA_PITCH_DEGREES: f32 = -89.;
const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (
                    // Suppressed in multiplayer — the client controls its predicted
                    // networked avatar, not a separate local player.
                    spawn.run_if(not(resource_exists::<crate::SuppressLocalPlayer>)),
                    // Camera self-heal (see the fn doc).
                    spawn_camera,
                    mouse_motion.after(EaseLabel),
                    toggle_holding
                        .in_set(ToggleHoldingSystemLabel)
                        .after(InputEvents)
                        // In multiplayer the netcode owns grab/attach; the local
                        // toggle would fight the mirrored Holding state.
                        .run_if(not(resource_exists::<crate::part::SuppressLocalParts>)),
                    // Single-player only: the fall→despawn→respawn cycle. In
                    // multiplayer the avatar is a lightyear-predicted entity the
                    // server owns — locally despawning it corrupts prediction, and
                    // the server respawns fallen avatars instead (`respawn_fallen_avatars`).
                    despawn.run_if(not(resource_exists::<crate::SuppressLocalPlayer>)),
                    attach_camera_orbit.in_set(AttachCameraOrbitSystem),
                    apply_part_rotation,
                    // The pickup camera/hold-point "feel" reacts to the shared `Holding`
                    // *state* (set by both single-player `toggle_holding` and the
                    // multiplayer predicted-grab path), so it works identically in both
                    // modes — see `adjust_camera_on_hold`.
                    (
                        adjust_camera_on_hold,
                        adjust_hold_point_on_hold.after(AttachCameraOrbitSystem),
                    )
                        .after(ToggleHoldingSystemLabel),
                    ease_camera.in_set(EaseLabel),
                    set_part_rotation.after(MouseWheelLabel),
                ),
            )
            .add_message::<PlayerClick>()
            .init_asset::<Config>()
            .add_message::<AttachEvent>()
            .init_resource::<CameraOrbitOffset>();
    }
}

// Bevy 0.12's asset rework replaced `TypeUuid` with the `Asset` derive
// (which still requires `TypePath`); the type id is derived, not a manual UUID.
#[derive(Asset, Deserialize, Copy, Clone, TypePath)]
pub struct Config {
    pub zoom_sensitivity: f32,
    look_sensitivity: f32,

    pub min_camera_distance: f32,
    pub max_camera_distance: f32,
    camera_offset_character_size_ratio: (f32, f32, f32),
}

#[derive(Bundle, Default)]
pub struct CameraOrbitCenterBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    camera_orbit_center: CameraOrbitCenter,
}

/// The orbit centre's offset from the character, in the LOOK frame (+y up,
/// +z toward the look direction). The character body never yaws (rotation-
/// locked; the look yaw lives on the orbit centre's rotation), so a raw child
/// translation is a fixed *world* offset — the rig would orbit an axis
/// displaced from the character. `mouse_motion` is the ONLY translation
/// writer: it composes `Ry(-yaw) * offset` into the translation each frame.
/// The pickup tween (`adjust_camera_on_hold`/`ease_camera`) eases this
/// offset, never the raw translation.
#[derive(Component)]
struct OrbitOffset(Vec3);

#[derive(Bundle, Default)]
pub struct HoldPointBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    hold_point: HoldPoint,
    original_position: OriginalPosition,
}

const BEVY_YAW_PITCH_ROLL_ANGLES: EulerRot = EulerRot::YXZ;

/// The camera's rest distance behind the orbit centre (metres). The scroll-zoom
/// (`client::input::zoom_camera`) moves it within the config's min/max; the launch
/// zoom-out multiplies it. The default the camera spawns at.
pub const DEFAULT_CAMERA_DISTANCE: f32 = 20.0;

/// Spawn the app-lifetime camera when none exists. Runs at startup *and* every
/// frame as a self-heal: in multiplayer the camera is mounted under the predicted
/// avatar's orbit hierarchy, and lightyear despawns that avatar recursively —
/// camera included — whenever the replicated world is torn down (most commonly a
/// websocket drop; the server can also despawn the avatar). No system can detach
/// the camera first (the despawn happens inside lightyear), so instead a fresh
/// camera is spawned at the boot pose and `attach_camera_orbit` +
/// `add_camera_to_player` re-mount it on the next avatar, exactly like app start.
fn spawn_camera(mut commands: Commands, cameras: Query<(), With<Camera>>) {
    if !cameras.is_empty() {
        return;
    }
    let mut camera_transform = Transform::from_rotation(Quat::from_euler(
        BEVY_YAW_PITCH_ROLL_ANGLES,
        std::f32::consts::PI,
        0.0,
        0.0,
    ));
    camera_transform.translation = -Vec3::Z * DEFAULT_CAMERA_DISTANCE;
    // Bevy 0.15 replaced `Camera3dBundle` with the `Camera3d` required-components
    // marker (it pulls in `Camera`, `Transform`, `Tonemapping`, etc.); spawn the
    // marker plus the components we want to override.
    commands.spawn((
        Camera3d::default(),
        camera_transform,
        // Bevy 0.11 changed the default tonemapper to TonyMcMapface, whose LUT
        // requires the `tonemapping_luts`/`ktx2`/`zstd` features (and embeds the
        // LUT in the wasm). Keep this minimal build small and preserve the prior
        // look by sticking with the 0.10 default, ReinhardLuminance.
        Tonemapping::ReinhardLuminance,
    ));
}

#[derive(Bundle, Default)]
struct PlayerBundle {
    player: Player,
    yaw: Yaw,
    look_pitch: LookPitch,
    keyboard_directional_input: KeyboardDirectionalInput,
    gamestick_directional_input: GameStickDirectionalInput,
    focused_interactable: FocusedInteractable,
    holding: Holding,
    part_rotation: PartRotation,
    clicked: LeftClicked,
}

fn spawn(mut commands: Commands, players: Query<(), With<Player>>) {
    if players.iter().next().is_none() {
        commands
            .spawn(PlayerBundle::default())
            .insert(PlayerInput::default());
    }
}

/// Turn an existing entity into the controllable local player (the input + camera
/// state — `Player`, `Yaw`/`LookPitch`, the directional/mouse input sinks). Used for
/// the client's *predicted* networked avatar: lightyear spawns it and we add the
/// player components so the existing input/camera/movement systems drive it. The
/// character body is added separately (`insert_character_body`); the camera attaches
/// via `attach_camera_orbit` once `Character` is present.
pub fn make_local_player(entity: &mut EntityCommands) {
    entity.insert((PlayerBundle::default(), PlayerInput::default()));
}

fn despawn(
    players: Query<(&Transform, Entity), With<Player>>,
    cameras: Query<Entity, With<Camera>>,
    mut commands: Commands,
) {
    for (player_transform, player_entity) in players.iter() {
        // Respawn as the player reaches the planet surface below the cliffs (2 m
        // above it, so they don't clip into the magma) — matching the multiplayer
        // `AVATAR_FALL_Y`. The planet is a visual with no collider; a fall off the
        // platform edge is caught here by height.
        if player_transform.translation.y < crate::map::PLANET_SURFACE_Y + 2.0 {
            // The single, app-lifetime camera is parented under the player's
            // orbit hierarchy. Bevy 0.16 made `despawn()` recursive, so detach
            // the camera first (clear its `ChildOf`) to keep it alive — it gets
            // re-parented to the next player by `add_camera_to_player`. Despawning
            // the player then clears the whole orbit-center/hold-point subtree.
            if let Some(camera) = cameras.iter().next() {
                commands.entity(camera).remove::<ChildOf>();
            }
            commands.entity(player_entity).despawn();
        }
    }
}

#[derive(Default, Resource)]
struct CameraOrbitOffset {
    min: Vec3,
}

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
struct AttachCameraOrbitSystem;

fn attach_camera_orbit(
    mut commands: Commands,
    // Attach the orbit hierarchy once, gated on *not having an orbit center yet* —
    // NOT on `Without<Children>`. The multiplayer predicted avatar already carries a
    // child (added by lightyear/avian), so a `Without<Children>` guard would never
    // fire for it and the camera would never mount. `PlayerCameraOrbitCenter` is set
    // below the moment we attach, so this stays idempotent for single-player too.
    characters_without_players: Query<
        (Entity, &GlobalTransform, &Collider),
        (With<Character>, Without<PlayerCameraOrbitCenter>),
    >,
    configs: ResMut<Assets<Config>>,
    mut camera_orbit_offset: ResMut<CameraOrbitOffset>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (character_entity, character_global_transform, character_collider) in
            characters_without_players.iter()
        {
            // This is simply a point that hovers above the character that the camera orbits around.
            // This is for the purpose of making it easier to see over obstructions.
            camera_orbit_offset.min = config.camera_offset_character_size_ratio.to_vec3()
                // Avian exposes the underlying parry shape via `.shape()` (rapier
                // used a public `.raw` field); the bounding-sphere call is parry's.
                * character_collider
                    .shape()
                    .compute_local_bounding_sphere()
                    .radius
                * 2.0;
            let mut camera_orbit_center_transform =
                Transform::from_translation(camera_orbit_offset.min);
            camera_orbit_center_transform.rotation = Quat::from_rotation_x(INITIAL_CAMERA_PITCH);
            let camera_orbit_center = commands
                .spawn((
                    CameraOrbitCenterBundle {
                        transform: camera_orbit_center_transform,
                        ..Default::default()
                    },
                    // See `OrbitOffset`. Yaw starts at 0, so the raw translation
                    // above matches the first composed one.
                    OrbitOffset(camera_orbit_offset.min),
                ))
                .id();

            let hold_point_transform = Transform::from_translation(Vec3::Z * 5.0);
            // GlobalTransform lost `translation_mut()` in Bevy 0.10; offset the
            // character's global transform through its affine instead.
            let mut hold_point_affine = character_global_transform.affine();
            hold_point_affine.translation += Vec3A::from(
                camera_orbit_center_transform.translation + hold_point_transform.translation,
            );
            let hold_point_global_transform = GlobalTransform::from(hold_point_affine);
            let hold_point = commands
                .spawn(HoldPointBundle {
                    transform: hold_point_transform.clone(),
                    global_transform: hold_point_global_transform,
                    original_position: OriginalPosition(hold_point_transform.translation),
                    ..Default::default()
                })
                .id();

            commands
                .entity(camera_orbit_center)
                .add_children(&[hold_point]);

            // Mount the rig on the player, with direct handles to both pieces —
            // the player's children may NOT be scanned positionally for them
            // (see `PlayerHoldPoint`).
            commands
                .entity(character_entity)
                .add_children(&[camera_orbit_center])
                .insert((
                    PlayerCameraOrbitCenter(camera_orbit_center),
                    PlayerHoldPoint(hold_point),
                ));
        }
    }
}

fn mouse_motion(
    time: Res<Time>,
    mut query: Query<(
        &mut Yaw,
        &mut LookPitch,
        &MouseMotionDelta,
        &Holding,
        &Modifying,
    )>,
    mut camera_orbit_center_transforms: Query<(&mut Transform, &OrbitOffset)>,
    configs: ResMut<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        if let Some((mut yaw, mut pitch, mouse_delta, holding, modifying)) = query.iter_mut().next()
        {
            if !(holding.0 && modifying.0) {
                yaw.0 = (yaw.0 + mouse_delta.0.x * time.delta_secs() * config.look_sensitivity)
                    % std::f32::consts::TAU;

                pitch.0 = (pitch.0
                    + mouse_delta.0.y * time.delta_secs() * config.look_sensitivity)
                    .max(MIN_CAMERA_PITCH)
                    .min(MAX_CAMERA_PITCH);
            }
            // The camera orbit center carries the full look orientation. Yaw used
            // to be applied by rotating the character body, but that body is a
            // rotation-locked physics body whose rotation the physics writeback
            // overwrites — so yaw is applied here too (the composition `Ry(-yaw) *
            // Rx(pitch)` matches the old `body(Ry(-yaw)) * orbit(Rx(pitch))`),
            // and the offset is rotated along with it (see `OrbitOffset`).
            // Compare-before-write: an identical write would still dirty the
            // orbit centre's whole subtree (camera, hold point) every frame.
            if let Some((mut transform, offset)) = camera_orbit_center_transforms.iter_mut().next()
            {
                let look_yaw = Quat::from_rotation_y(-yaw.0);
                let rotation = look_yaw * Quat::from_rotation_x(pitch.0);
                let translation = look_yaw * offset.0;
                if transform.rotation != rotation || transform.translation != translation {
                    transform.rotation = rotation;
                    transform.translation = translation;
                }
            }
        }
    }
}

#[derive(Bundle)]
struct HeldBundle {
    target_position: TargetPosition,
    target_orientation: TargetOrientation,
}

impl HeldBundle {
    fn new(hold_point: Entity, rotation: Quat) -> Self {
        Self {
            target_position: TargetPosition::new(hold_point),
            target_orientation: TargetOrientation::new(rotation),
        }
    }
}

fn toggle_holding(
    mut clicks: MessageReader<PlayerClick>,
    mut commands: Commands,
    mut players: Query<
        (&mut Holding, &FocusedInteractable, &PlayerHoldPoint, &Modifying),
        With<Player>,
    >,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    mut attach_events: MessageWriter<AttachEvent>,
) {
    if clicks.read().next().is_some() {
        if let Some((mut holding, interactable, hold_point, modifying)) =
            players.iter_mut().next()
        {
            if let Some(current_interactable) = interactable.0 {
                if let Ok(original_transform) = holdables.get(current_interactable) {
                    if holding.0 {
                        if modifying.0 {
                            attach_events.write(AttachEvent);
                        } else {
                            holding.0 = false;
                            commands
                                .entity(current_interactable)
                                .remove::<HeldBundle>();
                        }
                    } else if !modifying.0 {
                        holding.0 = true;
                        commands
                            .entity(current_interactable)
                            .insert(HeldBundle::new(
                                hold_point.0,
                                original_transform.compute_transform().rotation,
                            ));
                    }
                }
            }
        }
    }
}

/// A self-contained tween for the camera orbit center, replacing the former
/// `bevy_easings` dependency (which lagged Bevy releases). It eases the orbit
/// center's look-frame [`OrbitOffset`] from `start` to `end` over `duration`
/// seconds with a quadratic in-out curve (`mouse_motion` folds the offset into
/// the translation each frame); `ease_camera` advances it and removes the
/// component when the tween completes.
#[derive(Component)]
struct CameraTween {
    start: Vec3,
    end: Vec3,
    elapsed: f32,
    duration: f32,
}

const CAMERA_TWEEN_DURATION: f32 = 0.5;

impl CameraTween {
    fn new(start: Vec3, end: Vec3) -> Self {
        Self {
            start,
            end,
            elapsed: 0.0,
            duration: CAMERA_TWEEN_DURATION,
        }
    }
}

// Quadratic ease-in-out, matching the old `EaseFunction::QuadraticInOut`: accelerate
// over the first half, decelerate over the second.
fn quadratic_in_out(t: f32) -> f32 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
struct EaseLabel;

/// The bounding radius of the part currently held by `holding`/`focused`, or `0.0`
/// when not holding or nothing is focused (which eases the camera/hold-point back to
/// rest on release). The held entity is read from `FocusedInteractable` and its
/// `BoundingRadius` is attached by the shared `insert_part_physics`, so this resolves
/// in single-player (local part) and multiplayer (predicted replicated part) alike.
fn held_radius(
    holding: &Holding,
    focused: &FocusedInteractable,
    radiuses: &Query<&BoundingRadius>,
) -> f32 {
    if !holding.0 {
        return 0.0;
    }
    focused
        .0
        .and_then(|e| radiuses.get(e).ok())
        .map_or(0.0, |r| r.0)
}

/// Ease the camera-orbit centre up by the held part's bounding radius on pickup and
/// back down on release. This reacts to the **shared `Holding` state** — which both
/// the single-player `toggle_holding` and the multiplayer predicted-grab path
/// (`read_grab_intent`) set — rather than a `HoldEvent` that only the single-player
/// input system emits. Because that state is identical across modes, one system gives
/// single-player and multiplayer the same pickup feel with no mode-specific code.
/// (This is the general rule for keeping the two in sync: view/feel systems observe
/// replicated/shared *state*, never an event emitted inside a suppressed system.)
fn adjust_camera_on_hold(
    mut commands: Commands,
    changed: Query<(&Holding, &FocusedInteractable), (With<Player>, Changed<Holding>)>,
    camera_orbit_offset: Res<CameraOrbitOffset>,
    camera_orbit_centers: Query<(Entity, &OrbitOffset)>,
    radiuses: Query<&BoundingRadius>,
) {
    let Ok((holding, focused)) = changed.single() else {
        return;
    };
    let rise = held_radius(holding, focused, &radiuses);
    for (entity, offset) in camera_orbit_centers.iter() {
        commands.entity(entity).insert(CameraTween::new(
            offset.0,
            camera_orbit_offset.min + Vec3::Y * rise,
        ));
    }
}

fn ease_camera(
    mut commands: Commands,
    time: Res<Time>,
    mut cameras: Query<(Entity, &mut OrbitOffset, &mut CameraTween)>,
) {
    for (entity, mut offset, mut tween) in cameras.iter_mut() {
        tween.elapsed += time.delta_secs();
        let progress = (tween.elapsed / tween.duration).clamp(0.0, 1.0);
        offset.0 = tween.start.lerp(tween.end, quadratic_in_out(progress));
        // Tween done: snap exactly to the target and drop the component so it stops.
        if progress >= 1.0 {
            commands.entity(entity).remove::<CameraTween>();
        }
    }
}

/// Push the hold point out by the held part's bounding radius on pickup and back to
/// rest on release — the hold-point counterpart to `adjust_camera_on_hold`, driven by
/// the same shared `Holding` state so it works in both modes.
fn adjust_hold_point_on_hold(
    changed: Query<(&Holding, &FocusedInteractable), (With<Player>, Changed<Holding>)>,
    mut hold_points: Query<(&mut Transform, &OriginalPosition), With<HoldPoint>>,
    radiuses: Query<&BoundingRadius>,
) {
    let Ok((holding, focused)) = changed.single() else {
        return;
    };
    let push = held_radius(holding, focused, &radiuses);
    for (mut transform, original_position) in hold_points.iter_mut() {
        transform.translation = original_position.0 + Vec3::Z * push;
    }
}

fn set_part_rotation(
    mut players: Query<(&mut PartRotation, &PlayerCameraOrbitCenter, &Modifying, &MouseWheelDelta)>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    mouse_deltas: Query<&MouseMotionDelta>,
) {
    if let Some((mut rotation, orbit_center, modifying, mouse_wheel_delta)) =
        players.iter_mut().next()
    {
        rotation.0 = Quat::default();
        if modifying.0 {
            if let Ok(camera_orbit_center) = camera_orbit_centers.get(orbit_center.0) {
                let camera_orbit_center = camera_orbit_center.compute_transform();
                rotation.0 = Quat::from_axis_angle(
                    // `Transform::back` returns a direction type (`Dir3`
                    // since Bevy 0.14, formerly `Direction3d`); deref to `Vec3`.
                    *camera_orbit_center.back(),
                    mouse_wheel_delta.0 / 10.,
                ) * rotation.0;
                for mouse_delta in mouse_deltas.iter() {
                    if mouse_delta.0 != Vec2::ZERO {
                        let rotation_input = camera_orbit_center.rotation.mul_vec3(Vec3::new(
                            -mouse_delta.0.x,
                            -mouse_delta.0.y,
                            0.0,
                        ));
                        let rotation_axis =
                            rotation_input.cross(*camera_orbit_center.back()).normalize();
                        rotation.0 = Quat::from_axis_angle(
                            rotation_axis,
                            rotation_input.length() / 100.,
                        ) * rotation.0;
                    }
                }
            }
        }
    }
}

fn apply_part_rotation(
    players: Query<(&PartRotation, &Holding, &FocusedInteractable)>,
    mut parts: Query<&mut TargetOrientation>,
) {
    for (part_rotation, holding, focused_interactables) in players.iter() {
        if holding.0 {
            if let Some(focused_interactable) = focused_interactables.0 {
                if let Ok(mut orientation) = parts.get_mut(focused_interactable) {
                    orientation.quat = part_rotation.0 * orientation.quat;
                }
            }
        }
    }
}
