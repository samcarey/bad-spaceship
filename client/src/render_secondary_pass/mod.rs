use bad_spaceship_shared::{
    part::{Holdable, SuppressLocalParts, TargetOrientation, TargetPosition, DELETE_RADIUS},
    DisplayableJoint, ExistingJoints, HoldPoint, Holding, Modifying, Player, PlayerHoldPoint,
    PotentialJoints, PredeleteJoint, PredeleteJoints, UpdateJointsLabel,
};
// Bevy 0.17 moved `NotShadowCaster` from `bevy_pbr` to `bevy_light` (`bevy::light`).
use bevy::{asset::load_internal_asset, light::NotShadowCaster, prelude::*};
use normalization::*;

use self::gizmo_material::GizmoMaterial;

pub mod gizmo_material;

mod cone;
mod normalization;

pub struct RenderSecondaryPassPlugin;
impl Plugin for RenderSecondaryPassPlugin {
    fn build(&self, app: &mut App) {
        // Bevy 0.12 dropped `Assets::set_untracked`; the idiomatic way to embed
        // an internal shader is `load_internal_asset!`, which `include_str!`s the
        // source and registers it under the given weak handle.
        load_internal_asset!(
            app,
            gizmo_material::GIZMO_SHADER_HANDLE,
            "../../assets/gizmo_material.wgsl",
            Shader::from_wgsl
        );

        app.add_plugins((Ui3dNormalization, MaterialPlugin::<GizmoMaterial>::default()))
            .add_systems(Startup, (build_gizmo, initialize_joint_appearance))
            .init_resource::<JointAppearance>()
            .add_systems(
                Update,
                (
                    position_gizmo,
                    // The hold-point delete-zone sphere shows in multiplayer too:
                    // the predicted avatar carries the same `HoldPoint` child +
                    // `Holding`/`Modifying`, and joint deletion is now server-
                    // authoritative (`server_delete`), so the zone is meaningful.
                    add_hold_point_delete_zone_visualization,
                    (
                        display_potential_joints,
                        display_existing_joints,
                        display_predelete_joints,
                        delete_zone_visibility,
                    )
                        .after(UpdateJointsLabel),
                ),
            );
    }
}

/// Bevy 0.10 turned `Visibility` into an enum; this maps a bool onto the
/// explicit `Visible`/`Hidden` variants used throughout this module.
fn set_visible(visibility: &mut Visibility, visible: bool) {
    *visibility = if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
}

fn position_gizmo(
    helds: Query<(&TargetOrientation, &TargetPosition)>,
    hold_points: Query<&GlobalTransform, (With<HoldPoint>, Without<GizmoHub>)>,
    mut gizmo_hubs: Query<(&mut Transform, &mut Visibility, &Children), With<GizmoHub>>,
    mut gizmo_pieces: Query<&mut Visibility, (With<GizmoPiece>, Without<GizmoHub>)>,
    // In multiplayer the local hold is suppressed (no `TargetOrientation`/
    // `TargetPosition`), so there's nothing to drive the gizmo from `helds`.
    multiplayer: Option<Res<SuppressLocalParts>>,
    // Multiplayer: the held part's target orientation (tracked client-side, sent
    // to the server) and the mirrored `Holding` flag drive the gizmo instead.
    held_rotation: Option<Res<crate::net::HeldRotation>>,
    holding: Query<&Holding, With<Player>>,
) {
    let mut translation = None;
    let mut rotation = None;
    if let Some((target_orientation, target_position)) = helds.iter().next() {
        translation = match hold_points.get(target_position.hold_point_entity) {
            Ok(transform) => Some(transform.translation()),
            Err(_) => None,
        };
        rotation = Some(target_orientation.quat);
    } else if multiplayer.is_some() {
        // Multiplayer: while holding, place the gizmo at the hold point and orient
        // it to the held part's *target* orientation (the same value forwarded to
        // the server), so it indicates the orientation the part is driven toward —
        // matching the single-player gizmo. Hidden when not holding.
        let is_holding = holding.iter().next().is_some_and(|h| h.0);
        if let (true, Some(transform), Some(held_rotation)) =
            (is_holding, hold_points.iter().next(), held_rotation)
        {
            translation = Some(transform.translation());
            rotation = Some(held_rotation.0);
        }
    }

    let mut childs = Vec::new();
    let mut should_be_visible = false;

    if let Some((mut gizmo_transform, mut visible, children)) = gizmo_hubs.iter_mut().next() {
        if let Some(translation) = translation {
            if let Some(rotation) = rotation {
                gizmo_transform.translation = translation;
                gizmo_transform.rotation = rotation;
                should_be_visible = true;
            }
        }
        set_visible(&mut visible, should_be_visible);

        // Bevy 0.16's `Children::iter()` yields owned `Entity` values now.
        childs = children.iter().collect();
    }

    for child in childs {
        if let Ok(mut visible) = gizmo_pieces.get_mut(child) {
            set_visible(&mut visible, should_be_visible);
        }
    }
}

#[derive(Component)]
struct GizmoHub;

#[derive(Component)]
struct GizmoPiece;

#[derive(Bundle)]
pub struct TransformGizmoBundle {
    gc: GizmoHub,
    transform: Transform,
    // Bevy 0.15's required components mean `Visibility` now pulls in
    // `InheritedVisibility`/`ViewVisibility` and `Transform` pulls in
    // `GlobalTransform`, so they no longer need to be listed here.
    visibility: Visibility,
    normalize: Normalize3d,
}

impl Default for TransformGizmoBundle {
    fn default() -> Self {
        TransformGizmoBundle {
            gc: GizmoHub,
            transform: Transform::from_translation(Vec3::splat(f32::MIN)),
            visibility: Visibility::Hidden,
            normalize: Normalize3d,
        }
    }
}

/// Startup system that builds the procedural mesh and materials of the gizmo.
fn build_gizmo(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GizmoMaterial>>,
) {
    let axis_length = 1.5;
    // Define gizmo meshes
    // Bevy 0.13 deprecated `shape::*`; `Capsule3d::new(radius, length)` takes the
    // cylinder length directly (the old `depth`).
    let arrow_tail_mesh = meshes.add(Capsule3d::new(0.015, axis_length));
    let cone_mesh = meshes.add(Mesh::from(cone::Cone {
        height: 0.3,
        radius: 0.1,
        ..Default::default()
    }));
    // Define gizmo materials
    let (s, l, a) = (0.8, 0.5, 0.8);
    let gizmo_material_x = materials.add(GizmoMaterial::from(Color::hsla(0.0, s, l, a)));
    let gizmo_material_y = materials.add(GizmoMaterial::from(Color::hsla(120.0, s, l, a)));
    let gizmo_material_z = materials.add(GizmoMaterial::from(Color::hsla(240.0, s, l, a)));
    let gizmo_material_x_selectable = materials.add(GizmoMaterial::from(Color::hsl(0.0, s, l)));
    let gizmo_material_y_selectable = materials.add(GizmoMaterial::from(Color::hsl(120.0, s, l)));
    let gizmo_material_z_selectable = materials.add(GizmoMaterial::from(Color::hsl(240.0, s, l)));
    commands
        .spawn(TransformGizmoBundle::default())
        .with_children(|parent| {
            // Bevy 0.15: `MaterialMeshBundle` is replaced by the `Mesh3d` /
            // `MeshMaterial3d` required-components wrappers, and marker components
            // can ride along in the spawn tuple. Every gizmo piece is the same five
            // components differing only in mesh, material, and transform.
            let mut piece =
                |mesh: Handle<Mesh>, material: Handle<GizmoMaterial>, transform: Transform| {
                    parent.spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(material),
                        transform,
                        GizmoPiece,
                        NotShadowCaster,
                    ));
                };

            // Translation axis tails
            piece(
                arrow_tail_mesh.clone(),
                gizmo_material_x.clone(),
                Transform::from_matrix(Mat4::from_rotation_translation(
                    Quat::from_rotation_z(std::f32::consts::PI / 2.0),
                    Vec3::new(axis_length / 2.0, 0.0, 0.0),
                )),
            );
            piece(
                arrow_tail_mesh.clone(),
                gizmo_material_y.clone(),
                Transform::from_matrix(Mat4::from_rotation_translation(
                    Quat::from_rotation_y(std::f32::consts::PI / 2.0),
                    Vec3::new(0.0, axis_length / 2.0, 0.0),
                )),
            );
            piece(
                arrow_tail_mesh,
                gizmo_material_z.clone(),
                Transform::from_matrix(Mat4::from_rotation_translation(
                    Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                    Vec3::new(0.0, 0.0, axis_length / 2.0),
                )),
            );

            // Selectable cone heads
            piece(
                cone_mesh.clone(),
                gizmo_material_x_selectable.clone(),
                Transform::from_matrix(Mat4::from_rotation_translation(
                    Quat::from_rotation_z(std::f32::consts::PI / -2.0),
                    Vec3::new(axis_length, 0.0, 0.0),
                )),
            );
            piece(
                cone_mesh.clone(),
                gizmo_material_y_selectable.clone(),
                Transform::from_translation(Vec3::new(0.0, axis_length, 0.0)),
            );
            piece(
                cone_mesh.clone(),
                gizmo_material_z_selectable.clone(),
                Transform::from_matrix(Mat4::from_rotation_translation(
                    Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                    Vec3::new(0.0, 0.0, axis_length),
                )),
            );
        });
}

#[derive(Default, Resource)]
pub struct JointAppearance {
    pub mesh: Option<Handle<Mesh>>,
    /// Candidate (potential) joints — drawn **blue**.
    valid_material: Option<Handle<GizmoMaterial>>,
    /// The material both single-player and multiplayer draw *real* (existing)
    /// joints with — **green**.
    pub invalid_material: Option<Handle<GizmoMaterial>>,
    /// A joint inside the delete zone — drawn **red**. `pub` so the multiplayer
    /// recolor (`net::recolor_replicated_joints`) can swap a joint's own sphere to it.
    pub predelete_material: Option<Handle<GizmoMaterial>>,
}

fn initialize_joint_appearance(
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GizmoMaterial>>,
    mut joint_appearance: ResMut<JointAppearance>,
) {
    let (s, l, a) = (1.0, 0.5, 0.75);
    // Joint state → hue: candidate = blue (240°), real = green (120°),
    // delete-zone = red (0°).
    *joint_appearance = JointAppearance {
        mesh: Some(meshes.add(Sphere::new(0.1).mesh().ico(5).unwrap())),
        valid_material: Some(materials.add(GizmoMaterial::from(Color::hsla(240.0, s, l, a)))),
        invalid_material: Some(materials.add(GizmoMaterial::from(Color::hsla(120.0, s, l, a)))),
        predelete_material: Some(materials.add(GizmoMaterial::from(Color::hsla(0.0, s, l, a)))),
    };
}

#[derive(Component)]
struct DisplayedPotentialJoint;

fn display_potential_joints(
    mut commands: Commands,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    joints: Res<PotentialJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visibility), With<DisplayedPotentialJoint>>,
    displayed_joint_appearance: Res<JointAppearance>,
) {
    let mut display_points_iter = displayed_joints.iter_mut();
    for DisplayableJoint { points, entities } in joints.0.iter() {
        if let Ok(transform) = holdables.get(entities.0) {
            let transform = transform.compute_transform();
            let center = transform.translation + transform.rotation.mul_vec3(points.0.into());
            let material = displayed_joint_appearance.valid_material.clone().unwrap();

            if let Some((mut displayed_transform, mut displayed_visible)) =
                display_points_iter.next()
            {
                displayed_transform.translation = center;
                *displayed_visible = Visibility::Visible;
            } else {
                commands
                    .spawn((
                        Mesh3d(displayed_joint_appearance.mesh.clone().unwrap()),
                        MeshMaterial3d(material),
                        Transform::from_translation(center),
                    ))
                    .insert(DisplayedPotentialJoint)
                    .insert(NotShadowCaster);
            }
        }
    }
    for (_, mut displayed_visible) in display_points_iter {
        *displayed_visible = Visibility::Hidden;
    }
}

#[derive(Component)]
struct DisplayedExistingJoint;

fn display_existing_joints(
    mut commands: Commands,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    joints: Res<ExistingJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visibility), With<DisplayedExistingJoint>>,
    displayed_joint_appearance: Res<JointAppearance>,
) {
    let mut display_joints_iter = displayed_joints.iter_mut();
    for DisplayableJoint { points, entities } in joints.0.iter() {
        if let Ok(transform) = holdables.get(entities.0) {
            let transform = transform.compute_transform();
            let center = transform.translation + transform.rotation.mul_vec3(points.0.into());
            let material = displayed_joint_appearance.invalid_material.clone().unwrap();
            if let Some((mut displayed_transform, mut displayed_visible)) =
                display_joints_iter.next()
            {
                displayed_transform.translation = center;
                *displayed_visible = Visibility::Visible;
            } else {
                commands
                    .spawn((
                        Mesh3d(displayed_joint_appearance.mesh.clone().unwrap()),
                        MeshMaterial3d(material),
                        Transform::from_translation(center),
                    ))
                    .insert(DisplayedExistingJoint)
                    .insert(NotShadowCaster);
            }
        }
    }
    for (_, mut displayed_visible) in display_joints_iter {
        *displayed_visible = Visibility::Hidden;
    }
}

#[derive(Component)]
struct DisplayedPredeleteJoint;

fn display_predelete_joints(
    mut commands: Commands,
    joints: Res<PredeleteJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visibility), With<DisplayedPredeleteJoint>>,
    displayed_joint_appearance: Res<JointAppearance>,
) {
    let mut display_joints_iter = displayed_joints.iter_mut();
    for PredeleteJoint { translation, .. } in joints.0.iter() {
        let center = translation.clone();
        let material = displayed_joint_appearance
            .predelete_material
            .clone()
            .unwrap();
        if let Some((mut displayed_transform, mut displayed_visible)) = display_joints_iter.next() {
            displayed_transform.translation = center;
            *displayed_visible = Visibility::Visible;
        } else {
            commands
                .spawn((
                    Mesh3d(displayed_joint_appearance.mesh.clone().unwrap()),
                    MeshMaterial3d(material),
                    Transform::from_translation(center),
                ))
                .insert(DisplayedPredeleteJoint)
                .insert(NotShadowCaster);
        }
    }
    for (_, mut displayed_visible) in display_joints_iter {
        *displayed_visible = Visibility::Hidden;
    }
}

fn add_hold_point_delete_zone_visualization(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    hold_points_without_visualization: Query<Entity, (With<HoldPoint>, Without<Visibility>)>,
) {
    if let Some(entity) = hold_points_without_visualization.iter().next() {
        commands
            .entity(entity)
            .insert((
                Visibility::Hidden,
                Mesh3d(meshes.add(Sphere::new(DELETE_RADIUS).mesh().ico(5).unwrap())),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::hsla(20.0, 1.0, 0.3, 0.25),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..Default::default()
                })),
            ))
            .insert(NotShadowCaster);
    }
}

fn delete_zone_visibility(
    players: Query<(&Holding, &Modifying, &PlayerHoldPoint)>,
    mut hold_points: Query<&mut Visibility, With<HoldPoint>>,
) {
    if let Some((holding, modifying, hold_point)) = players.iter().next() {
        if let Ok(mut visible) = hold_points.get_mut(hold_point.0) {
            set_visible(&mut visible, modifying.0 && !holding.0);
        }
    }
}
