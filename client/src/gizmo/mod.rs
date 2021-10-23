use bad_spaceship_shared::{
    part::{TargetOrientation, TargetPosition},
    HoldPoint,
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
            .add_plugin(normalization::Ui3dNormalization);
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
