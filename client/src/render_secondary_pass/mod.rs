use bad_spaceship_shared::{
    part::{Holdable, TargetOrientation, TargetPosition, DELETE_RADIUS},
    player::get_hold_point_entity,
    DisplayableJoint, ExistingJoints, HoldPoint, Holding, Modifying, PotentialJoints,
    PredeleteJoint, PredeleteJoints, UpdateJointsLabel,
};
use bevy::{pbr::NotShadowCaster, prelude::*};
use normalization::*;

use self::gizmo_material::GizmoMaterial;

mod gizmo_material;

mod cone;
mod normalization;

pub struct RenderSecondaryPassPlugin;
impl Plugin for RenderSecondaryPassPlugin {
    fn build(&self, app: &mut App) {
        let mut shaders = app.world.get_resource_mut::<Assets<Shader>>().unwrap();
        shaders.set_untracked(
            gizmo_material::GIZMO_SHADER_HANDLE,
            // Bevy 0.11's naga_oil importer needs a path for diagnostics/imports.
            Shader::from_wgsl(
                include_str!("../../assets/gizmo_material.wgsl"),
                "gizmo_material.wgsl",
            ),
        );

        app.add_plugins((Ui3dNormalization, MaterialPlugin::<GizmoMaterial>::default()))
            .add_systems(Startup, (build_gizmo, initialize_joint_appearance))
            .init_resource::<JointAppearance>()
            .add_systems(
                Update,
                (
                    position_gizmo,
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
) {
    let mut translation = None;
    let mut rotation = None;
    if let Some((target_orientation, target_position)) = helds.iter().next() {
        translation = match hold_points.get(target_position.hold_point_entity) {
            Ok(transform) => Some(transform.translation()),
            Err(_) => None,
        };
        rotation = Some(target_orientation.quat);
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

        childs = children.iter().cloned().collect();
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
    global_transform: GlobalTransform,
    visible: Visibility,
    computed_visibility: ComputedVisibility,
    normalize: Normalize3d,
}

impl Default for TransformGizmoBundle {
    fn default() -> Self {
        TransformGizmoBundle {
            gc: GizmoHub,
            transform: Transform::from_translation(Vec3::splat(f32::MIN)),
            visible: Visibility::Hidden,
            computed_visibility: ComputedVisibility::default(),
            global_transform: GlobalTransform::default(),
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
    let arrow_tail_mesh = meshes.add(Mesh::from(shape::Capsule {
        radius: 0.015,
        depth: axis_length,
        ..Default::default()
    }));
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
            // Translation Axes
            parent
                .spawn(MaterialMeshBundle {
                    mesh: arrow_tail_mesh.clone(),
                    material: gizmo_material_x.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_z(std::f32::consts::PI / 2.0),
                        Vec3::new(axis_length / 2.0, 0.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);
            parent
                .spawn(MaterialMeshBundle {
                    mesh: arrow_tail_mesh.clone(),
                    material: gizmo_material_y.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_y(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, axis_length / 2.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);
            parent
                .spawn(MaterialMeshBundle {
                    mesh: arrow_tail_mesh,
                    material: gizmo_material_z.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, 0.0, axis_length / 2.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);

            parent
                .spawn(MaterialMeshBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_x_selectable.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_z(std::f32::consts::PI / -2.0),
                        Vec3::new(axis_length, 0.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);
            parent
                .spawn(MaterialMeshBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_y_selectable.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, axis_length, 0.0)),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);
            parent
                .spawn(MaterialMeshBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_z_selectable.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, 0.0, axis_length),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPiece)
                .insert(NotShadowCaster);
        });
}

#[derive(Default, Resource)]
struct JointAppearance {
    mesh: Option<Handle<Mesh>>,
    valid_material: Option<Handle<GizmoMaterial>>,
    invalid_material: Option<Handle<GizmoMaterial>>,
    predelete_material: Option<Handle<GizmoMaterial>>,
}

fn initialize_joint_appearance(
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GizmoMaterial>>,
    mut joint_appearance: ResMut<JointAppearance>,
) {
    let (s, l, a) = (1.0, 0.5, 0.75);
    *joint_appearance = JointAppearance {
        mesh: Some(
            meshes.add(
                Mesh::try_from(shape::Icosphere {
                    radius: 0.1,
                    ..Default::default()
                })
                .unwrap(),
            ),
        ),
        valid_material: Some(materials.add(GizmoMaterial::from(Color::hsla(260.0, s, l, a)))),
        invalid_material: Some(materials.add(GizmoMaterial::from(Color::hsla(20.0, s, l, a)))),
        predelete_material: Some(materials.add(GizmoMaterial::from(Color::hsla(20.0, s, l, a)))),
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
                    .spawn(MaterialMeshBundle {
                        mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                        material,
                        transform: Transform::from_translation(center),
                        ..Default::default()
                    })
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
                    .spawn(MaterialMeshBundle {
                        mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                        material,
                        transform: Transform::from_translation(center),
                        ..Default::default()
                    })
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
                .spawn(MaterialMeshBundle {
                    mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                    material,
                    transform: Transform::from_translation(center),
                    ..Default::default()
                })
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
            .insert(MaterialMeshBundle {
                visibility: Visibility::Hidden,
                mesh: meshes.add(
                    Mesh::try_from(shape::Icosphere {
                        radius: DELETE_RADIUS,
                        ..Default::default()
                    })
                    .unwrap(),
                ),
                material: materials.add(StandardMaterial {
                    base_color: Color::hsla(20.0, 1.0, 0.3, 0.25),
                    alpha_mode: AlphaMode::Blend,
                    unlit: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .insert(NotShadowCaster);
    }
}

fn delete_zone_visibility(
    players: Query<(&Holding, &Modifying, &Children)>,
    hold_points0: Query<(), With<HoldPoint>>,
    mut hold_points1: Query<&mut Visibility, With<HoldPoint>>,
    // mut hold_points: QuerySet<(
    //     QueryState<(), With<HoldPoint>>,
    //     QueryState<&mut Visibility, With<HoldPoint>>,
    // )>,
    camera_orbit_centers: Query<&Children>,
) {
    if let Some((holding, modifying, player_children)) = players.iter().next() {
        if let Some(entity) =
            get_hold_point_entity(player_children, camera_orbit_centers, &hold_points0)
        {
            if let Ok(mut visible) = hold_points1.get_mut(entity) {
                set_visible(&mut visible, modifying.0 && !holding.0);
            }
        }
    }
}
