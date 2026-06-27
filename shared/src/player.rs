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
    GameStickDirectionalInput, HoldEvent, HoldPoint, Holding, InputEvents,
    KeyboardDirectionalInput, LeftClicked, LookPitch, Modifying, MouseMotionDelta, MouseWheelDelta,
    MouseWheelLabel, OriginalPosition, PartRotation, Player, PlayerCameraOrbitCenter, PlayerClick,
    PlayerInput, ReleaseEvent, ToggleHoldingSystemLabel, Yaw, INITIAL_CAMERA_PITCH,
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
                    mouse_motion.after(EaseLabel),
                    toggle_holding
                        .in_set(ToggleHoldingSystemLabel)
                        .after(InputEvents)
                        // In multiplayer the netcode owns grab/attach; the local
                        // toggle would fight the mirrored Holding state.
                        .run_if(not(resource_exists::<crate::part::SuppressLocalParts>)),
                    despawn,
                    attach_camera_orbit.in_set(AttachCameraOrbitSystem),
                    apply_part_rotation,
                    (
                        reset_camera_after_release,
                        adjust_camera_on_hold,
                        reset_hold_point_after_release.after(AttachCameraOrbitSystem),
                        adjust_hold_point_on_hold,
                    )
                        .after(ToggleHoldingSystemLabel),
                    ease_camera.in_set(EaseLabel),
                    set_part_rotation.after(MouseWheelLabel),
                ),
            )
            .add_message::<PlayerClick>()
            .init_asset::<Config>()
            .add_message::<AttachEvent>()
            .add_message::<ReleaseEvent>()
            .init_resource::<CameraOrbitOffset>()
            .add_message::<HoldEvent>();
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

#[derive(Bundle, Default)]
pub struct HoldPointBundle {
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    hold_point: HoldPoint,
    original_position: OriginalPosition,
}

const BEVY_YAW_PITCH_ROLL_ANGLES: EulerRot = EulerRot::YXZ;

fn spawn_camera(mut commands: Commands) {
    let mut camera_transform = Transform::from_rotation(Quat::from_euler(
        BEVY_YAW_PITCH_ROLL_ANGLES,
        std::f32::consts::PI,
        0.0,
        0.0,
    ));
    camera_transform.translation = -Vec3::Z * 20.0;
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
        if player_transform.translation.y < -30.0 {
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
    characters_without_players: Query<
        (Entity, &GlobalTransform, &Collider),
        (With<Character>, Without<Children>),
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
                .spawn(CameraOrbitCenterBundle {
                    transform: camera_orbit_center_transform,
                    ..Default::default()
                })
                .id();

            // Mount the camera center to the player
            commands
                .entity(character_entity)
                .add_children(&[camera_orbit_center])
                .insert(PlayerCameraOrbitCenter(camera_orbit_center));

            // Mount the camera to the camera orbit center
            // commands
            //     .entity(camera_orbit_center)
            //     .push_children(&[camera]);

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
    mut camera_orbit_center_transforms: Query<&mut Transform, With<CameraOrbitCenter>>,
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
            // Rapier-owned ROTATION_LOCKED ball whose rotation the physics writeback
            // overwrites — so yaw is applied here too (the composition `Ry(-yaw) *
            // Rx(pitch)` matches the old `body(Ry(-yaw)) * orbit(Rx(pitch))`).
            if let Some(mut transform) = camera_orbit_center_transforms.iter_mut().next() {
                transform.rotation =
                    Quat::from_rotation_y(-yaw.0) * Quat::from_rotation_x(pitch.0);
            }
        }
    }
}

pub fn get_hold_point_entity(
    player_children: &Children,
    camera_orbit_centers: Query<&Children>,
    hold_points: &Query<(), With<HoldPoint>>,
) -> Option<Entity> {
    // TODO: eliminate need for this function
    let mut held_entity: Option<Entity> = None;
    // Bevy 0.16's `Children::iter()` (a `RelationshipTarget` method) yields
    // `Entity` by value now, not `&Entity`, so these no longer need dereferencing.
    if let Some(camera_orbit_center) = player_children.iter().next() {
        if let Ok(potential_hold_points) = camera_orbit_centers.get(camera_orbit_center) {
            for potential_hold_point in potential_hold_points.iter() {
                if hold_points.get(potential_hold_point).is_ok() {
                    held_entity = Some(potential_hold_point);
                }
            }
        }
    }
    held_entity
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
    mut players: Query<(&mut Holding, &FocusedInteractable, &Children, &Modifying), With<Player>>,
    camera_orbit_centers: Query<&Children>,
    hold_points: Query<(), With<HoldPoint>>,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    mut attach_events: MessageWriter<AttachEvent>,
    mut release_events: MessageWriter<ReleaseEvent>,
    mut hold_events: MessageWriter<HoldEvent>,
) {
    if clicks.read().next().is_some() {
        if let Some((mut holding, interactable, player_children, modifying)) =
            players.iter_mut().next()
        {
            if let Some(current_interactable) = interactable.0 {
                if let Ok(original_transform) = holdables.get(current_interactable) {
                    if let Some(hold_point_entity) =
                        get_hold_point_entity(player_children, camera_orbit_centers, &hold_points)
                    {
                        if holding.0 {
                            if modifying.0 {
                                attach_events.write(AttachEvent);
                            } else {
                                holding.0 = false;
                                commands
                                    .entity(current_interactable)
                                    .remove::<HeldBundle>();
                                release_events.write(ReleaseEvent);
                            }
                        } else if !modifying.0 {
                            holding.0 = true;
                            commands
                                .entity(current_interactable)
                                .insert(HeldBundle::new(
                                    hold_point_entity,
                                    original_transform.compute_transform().rotation,
                                ));
                            hold_events.write(HoldEvent {
                                held: current_interactable,
                            });
                        }
                    }
                }
            }
        }
    }
}

/// A self-contained translation tween for the camera orbit center, replacing the
/// former `bevy_easings` dependency (which lagged Bevy releases). It eases the
/// orbit center's `Transform.translation` from `start` to `end` over `duration`
/// seconds with a quadratic in-out curve; `ease_camera` advances it and removes
/// the component when the tween completes.
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

fn adjust_camera_on_hold(
    mut commands: Commands,
    mut hold_events: MessageReader<HoldEvent>,
    camera_orbit_offset: Res<CameraOrbitOffset>,
    camera_orbit_centers: Query<(Entity, &Transform), With<CameraOrbitCenter>>,
    radiuses: Query<&BoundingRadius, With<Holdable>>,
) {
    if let Some(hold_event) = hold_events.read().next() {
        if let Ok(radius) = radiuses.get(hold_event.held) {
            for (entity, transform) in camera_orbit_centers.iter() {
                commands.entity(entity).insert(CameraTween::new(
                    transform.translation,
                    camera_orbit_offset.min + Vec3::Y * radius.0,
                ));
            }
        }
    }
}

fn reset_camera_after_release(
    mut commands: Commands,
    mut release_events: MessageReader<ReleaseEvent>,
    camera_orbit_offset: ResMut<CameraOrbitOffset>,
    mut camera_orbit_centers: Query<(Entity, &mut Transform), With<CameraOrbitCenter>>,
) {
    if release_events.read().next().is_some() {
        for (entity, transform) in camera_orbit_centers.iter_mut() {
            commands
                .entity(entity)
                .insert(CameraTween::new(transform.translation, camera_orbit_offset.min));
        }
    }
}

fn ease_camera(
    mut commands: Commands,
    time: Res<Time>,
    mut cameras: Query<(Entity, &mut Transform, &mut CameraTween), With<CameraOrbitCenter>>,
) {
    for (entity, mut transform, mut tween) in cameras.iter_mut() {
        tween.elapsed += time.delta_secs();
        let progress = (tween.elapsed / tween.duration).clamp(0.0, 1.0);
        transform.translation = tween.start.lerp(tween.end, quadratic_in_out(progress));
        // Tween done: snap exactly to the target and drop the component so it stops.
        if progress >= 1.0 {
            commands.entity(entity).remove::<CameraTween>();
        }
    }
}

fn adjust_hold_point_on_hold(
    mut hold_events: MessageReader<HoldEvent>,
    mut hold_points: Query<(&mut Transform, &OriginalPosition), With<HoldPoint>>,
    radiuses: Query<&BoundingRadius, With<Holdable>>,
) {
    if let Some(hold_event) = hold_events.read().next() {
        if let Ok(radius) = radiuses.get(hold_event.held) {
            for (mut transform, original_position) in hold_points.iter_mut() {
                transform.translation = original_position.0 + Vec3::Z * radius.0;
            }
        }
    }
}

fn reset_hold_point_after_release(
    mut release_events: MessageReader<ReleaseEvent>,
    mut hold_points: Query<(&mut Transform, &OriginalPosition), With<HoldPoint>>,
) {
    if release_events.read().next().is_some() {
        for (mut transform, original_position) in hold_points.iter_mut() {
            transform.translation = original_position.0;
        }
    }
}

fn set_part_rotation(
    mut players: Query<(&mut PartRotation, &Children, &Modifying, &MouseWheelDelta)>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
    mouse_deltas: Query<&MouseMotionDelta>,
) {
    if let Some((mut rotation, player_children, modifying, mouse_wheel_delta)) =
        players.iter_mut().next()
    {
        rotation.0 = Quat::default();
        if modifying.0 {
            for child in player_children.iter() {
                if let Ok(camera_orbit_center) = camera_orbit_centers.get(child) {
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
