// Bevy 0.17 standardised system-set names on the `*Systems` suffix:
// `TransformSystem` → `TransformSystems`. `Camera` comes from the prelude now
// (it moved to the `bevy_camera` crate, but the prelude re-export is unchanged).
use bevy::{prelude::*, transform::TransformSystems};

pub struct Ui3dNormalization;
impl Plugin for Ui3dNormalization {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            normalize.before(TransformSystems::Propagate),
        );
    }
}

/// Marker struct that marks entities with meshes that should be scaled relative to the camera.
#[derive(Component)]
pub struct Normalize3d;

#[allow(clippy::type_complexity)]
pub fn normalize(
    camera_query: Query<&GlobalTransform, With<Camera>>,
    mut normalize_query: Query<&mut Transform, With<Normalize3d>>,
) {
    // TODO: can be improved by manually specifying the active camera to normalize against. The
    // majority of cases will only use a single camera for this viewer, so this is sufficient.
    //
    // Camera-less frames are real, not a bug to assert away: in multiplayer the
    // camera rides the predicted avatar's subtree, which lightyear tears down on
    // disconnect; the camera self-heal (`spawn_camera`) restores it a frame later.
    // On wasm (panic = abort) an `expect` here froze the whole game on any
    // websocket drop.
    let Some(camera_position) = camera_query.iter().last().cloned() else {
        return;
    };

    for mut transform in normalize_query.iter_mut() {
        // Bevy 0.17 renamed `GlobalTransform::compute_matrix` → `to_matrix`.
        let distance = -camera_position
            .to_matrix()
            .inverse()
            .transform_point3(transform.translation)
            .z;

        transform.scale = Vec3::splat(distance / 50.0);
    }
}
