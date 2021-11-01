use bad_spaceship_shared::{
    part::{Holdable, TargetOrientation, TargetPosition, DELETE_RADIUS},
    player::get_hold_point_entity,
    DeletingJoint, DisplayableJoint, ExistingJoints, HoldPoint, Holding, PotentialJoints,
    PredeleteJoint, PredeleteJoints, UpdateAttachPointsLabel,
};
use bevy::{prelude::*, render::render_graph::base::MainPass};
use normalization::*;
use render_graph::GizmoPass;

mod cone;
mod normalization;
mod render_graph;
mod truncated_torus;

pub struct TransformGizmoPlugin;
impl Plugin for TransformGizmoPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(build_gizmo.system())
            .add_system(position_gizmo.system())
            .add_plugin(normalization::Ui3dNormalization)
            .add_startup_system(initialize_attach_point.system())
            .init_resource::<AttachPointAppearance>()
            .add_system(add_hold_point_delete_zone_visualization.system())
            .add_system_set(
                SystemSet::new()
                    .after(UpdateAttachPointsLabel)
                    .with_system(display_potential_joints.system())
                    .with_system(display_existing_joints.system())
                    .with_system(display_predelete_joints.system())
                    .with_system(delete_zone_visibility.system()),
            );
        {
            render_graph::add_gizmo_graph(&mut app.world_mut());
        }
    }
}

fn position_gizmo(
    helds: Query<(&TargetOrientation, &TargetPosition)>,

    mut transforms: QuerySet<(
        Query<(&mut Transform, &mut Visible, &Children), With<GizmoComponent>>,
        Query<&GlobalTransform, With<HoldPoint>>,
        Query<&mut Visible>,
    )>,
) {
    let mut translation = None;
    let mut rotation = None;
    if let Some((target_orientation, target_position)) = helds.iter().next() {
        translation = match transforms.q1().get(target_position.hold_point_entity) {
            Ok(transform) => Some(transform.translation),
            Err(_) => None,
        };
        rotation = Some(target_orientation.quat);
    }

    let mut childs = Vec::new();
    let mut should_be_visible = false;

    if let Some((mut gizmo_transform, mut visible, children)) =
        transforms.q0_mut().iter_mut().next()
    {
        if let Some(translation) = translation {
            if let Some(rotation) = rotation {
                gizmo_transform.translation = translation;
                gizmo_transform.rotation = rotation;
                should_be_visible = true;
            }
        }
        visible.is_visible = should_be_visible;

        childs = children.iter().cloned().collect();
    }

    for child in childs {
        if let Ok(mut visible) = transforms.q2_mut().get_mut(child) {
            visible.is_visible = should_be_visible;
        }
    }
}

struct GizmoComponent;

#[derive(Bundle)]
pub struct TransformGizmoBundle {
    gc: GizmoComponent,
    transform: Transform,
    global_transform: GlobalTransform,
    visible: Visible,
    normalize: Normalize3d,
}

impl Default for TransformGizmoBundle {
    fn default() -> Self {
        TransformGizmoBundle {
            gc: GizmoComponent,
            transform: Transform::from_translation(Vec3::splat(f32::MIN)),
            visible: Visible {
                is_visible: false,
                ..Default::default()
            },
            global_transform: GlobalTransform::default(),
            normalize: Normalize3d,
        }
    }
}

/// Startup system that builds the procedural mesh and materials of the gizmo.
fn build_gizmo(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
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
    let gizmo_material_x = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(1.0, 0.4, 0.4),
        ..Default::default()
    });
    let gizmo_material_y = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(0.4, 1.0, 0.4),
        ..Default::default()
    });
    let gizmo_material_z = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(0.4, 0.5, 1.0),
        ..Default::default()
    });
    let gizmo_material_x_selectable = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(1.0, 0.7, 0.7),
        ..Default::default()
    });
    let gizmo_material_y_selectable = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(0.7, 1.0, 0.7),
        ..Default::default()
    });
    let gizmo_material_z_selectable = materials.add(StandardMaterial {
        unlit: true,
        base_color: Color::rgb(0.7, 0.7, 1.0),
        ..Default::default()
    });
    commands
        .spawn_bundle(TransformGizmoBundle::default())
        .with_children(|parent| {
            // Translation Axes
            parent
                .spawn_bundle(PbrBundle {
                    mesh: arrow_tail_mesh.clone(),
                    material: gizmo_material_x.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_z(std::f32::consts::PI / 2.0),
                        Vec3::new(axis_length / 2.0, 0.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();
            parent
                .spawn_bundle(PbrBundle {
                    mesh: arrow_tail_mesh.clone(),
                    material: gizmo_material_y.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_y(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, axis_length / 2.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();
            parent
                .spawn_bundle(PbrBundle {
                    mesh: arrow_tail_mesh,
                    material: gizmo_material_z.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, 0.0, axis_length / 2.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();

            // Translation Handles
            parent
                .spawn_bundle(PbrBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_x_selectable.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_z(std::f32::consts::PI / -2.0),
                        Vec3::new(axis_length, 0.0, 0.0),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();
            parent
                .spawn_bundle(PbrBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_y_selectable.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, axis_length, 0.0)),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();
            parent
                .spawn_bundle(PbrBundle {
                    mesh: cone_mesh.clone(),
                    material: gizmo_material_z_selectable.clone(),
                    transform: Transform::from_matrix(Mat4::from_rotation_translation(
                        Quat::from_rotation_x(std::f32::consts::PI / 2.0),
                        Vec3::new(0.0, 0.0, axis_length),
                    )),
                    ..Default::default()
                })
                .insert(GizmoPass)
                .remove::<MainPass>();
        })
        .insert(GizmoPass)
        .remove::<MainPass>();
}

#[derive(Default)]
struct AttachPointAppearance {
    mesh: Option<Handle<Mesh>>,
    valid_material: Option<Handle<StandardMaterial>>,
    invalid_material: Option<Handle<StandardMaterial>>,
    predelete_material: Option<Handle<StandardMaterial>>,
}

fn initialize_attach_point(
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut attach_point_appearance: ResMut<AttachPointAppearance>,
) {
    *attach_point_appearance = AttachPointAppearance {
        mesh: Some(meshes.add(Mesh::from(shape::Icosphere {
            radius: 0.1,
            ..Default::default()
        }))),
        valid_material: Some(materials.add(StandardMaterial {
            unlit: true,
            base_color: Color::rgb(1.0, 1.0, 0.2),
            ..Default::default()
        })),
        invalid_material: Some(materials.add(StandardMaterial {
            unlit: true,
            base_color: Color::rgb(0.4, 0.4, 1.0),
            ..Default::default()
        })),
        predelete_material: Some(materials.add(StandardMaterial {
            unlit: true,
            base_color: Color::rgb(1.0, 0.4, 0.4),
            ..Default::default()
        })),
    };
}

struct DisplayedPotentialJoint;

fn display_potential_joints(
    mut commands: Commands,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    joints: Res<PotentialJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visible), With<DisplayedPotentialJoint>>,
    displayed_joint_appearance: Res<AttachPointAppearance>,
) {
    let mut display_points_iter = displayed_joints.iter_mut();
    for DisplayableJoint { points, entities } in joints.0.iter() {
        if let Ok(transform) = holdables.get(entities.0) {
            let center = transform.translation + transform.rotation.mul_vec3(points.0.into());
            let material = displayed_joint_appearance.valid_material.clone().unwrap();

            if let Some((mut displayed_transform, mut displayed_visible)) =
                display_points_iter.next()
            {
                displayed_transform.translation = center;
                displayed_visible.is_visible = true;
            } else {
                commands
                    .spawn_bundle(PbrBundle {
                        mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                        material,
                        transform: Transform::from_translation(center),
                        ..Default::default()
                    })
                    .insert(DisplayedPotentialJoint)
                    .insert(GizmoPass)
                    .remove::<MainPass>();
            }
        }
    }
    for (_, mut displayed_visible) in display_points_iter {
        displayed_visible.is_visible = false;
    }
}

struct DisplayedExistingJoint;

fn display_existing_joints(
    mut commands: Commands,
    holdables: Query<&GlobalTransform, With<Holdable>>,
    joints: Res<ExistingJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visible), With<DisplayedExistingJoint>>,
    displayed_joint_appearance: Res<AttachPointAppearance>,
) {
    let mut display_joints_iter = displayed_joints.iter_mut();
    for DisplayableJoint { points, entities } in joints.0.iter() {
        if let Ok(transform) = holdables.get(entities.0) {
            let center = transform.translation + transform.rotation.mul_vec3(points.0.into());
            let material = displayed_joint_appearance.invalid_material.clone().unwrap();
            if let Some((mut displayed_transform, mut displayed_visible)) =
                display_joints_iter.next()
            {
                displayed_transform.translation = center;
                displayed_visible.is_visible = true;
            } else {
                commands
                    .spawn_bundle(PbrBundle {
                        mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                        material,
                        transform: Transform::from_translation(center),
                        ..Default::default()
                    })
                    .insert(DisplayedExistingJoint)
                    .insert(GizmoPass)
                    .remove::<MainPass>();
            }
        }
    }
    for (_, mut displayed_visible) in display_joints_iter {
        displayed_visible.is_visible = false;
    }
}

struct DisplayedPredeleteJoint;

fn display_predelete_joints(
    mut commands: Commands,
    joints: Res<PredeleteJoints>,
    mut displayed_joints: Query<(&mut Transform, &mut Visible), With<DisplayedPredeleteJoint>>,
    displayed_joint_appearance: Res<AttachPointAppearance>,
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
            displayed_visible.is_visible = true;
        } else {
            commands
                .spawn_bundle(PbrBundle {
                    mesh: displayed_joint_appearance.mesh.clone().unwrap(),
                    material,
                    transform: Transform::from_translation(center),
                    ..Default::default()
                })
                .insert(DisplayedPredeleteJoint)
                .insert(GizmoPass)
                .remove::<MainPass>();
        }
    }
    for (_, mut displayed_visible) in display_joints_iter {
        displayed_visible.is_visible = false;
    }
}

fn add_hold_point_delete_zone_visualization(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    hold_points_without_visualization: Query<Entity, (With<HoldPoint>, Without<Visible>)>,
) {
    if let Some(entity) = hold_points_without_visualization.iter().next() {
        commands
            .entity(entity)
            .insert_bundle(PbrBundle {
                visible: Visible {
                    is_visible: false,
                    is_transparent: true,
                },
                mesh: meshes.add(
                    shape::Icosphere {
                        radius: DELETE_RADIUS,
                        ..Default::default()
                    }
                    .into(),
                ),
                material: materials.add(StandardMaterial {
                    base_color: Color::rgba(0.6, 0.5, 0.0, 0.25),
                    roughness: 1.0,
                    unlit: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .insert(GizmoPass)
            .remove::<MainPass>();
    }
}

fn delete_zone_visibility(
    players: Query<(&Holding, &DeletingJoint, &Children)>,
    mut hold_points: QuerySet<(
        Query<(), With<HoldPoint>>,
        Query<&mut Visible, With<HoldPoint>>,
    )>,
    camera_orbit_centers: Query<&Children>,
) {
    if let Some((holding, deleting, player_children)) = players.iter().next() {
        if let Some(entity) =
            get_hold_point_entity(player_children, camera_orbit_centers, hold_points.q0())
        {
            if let Ok(mut visible) = hold_points.q1_mut().get_mut(entity) {
                visible.is_visible = deleting.0 && !holding.0;
            }
        }
    }
}
