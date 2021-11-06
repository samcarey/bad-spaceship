use bad_spaceship_shared::part::Holdable;
use bevy::prelude::*;
use rand::Rng;

pub struct MaterialsPlugin;

impl Plugin for MaterialsPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(assign_parts.system());
    }
}

struct AssignedMaterial;

const COLOR_MIN: f32 = 0.2;
const COLOR_MAX: f32 = 0.7;

fn assign_parts(
    mut commands: Commands,
    unassigned: Query<
        (Entity, &Handle<StandardMaterial>),
        (With<Holdable>, Without<AssignedMaterial>),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut rng = rand::thread_rng();
    for (entity, material_handle) in unassigned.iter() {
        let material = &mut materials.get_mut(&*material_handle).unwrap();
        material.base_color = Color::rgba(
            rng.gen_range(COLOR_MIN..=COLOR_MAX),
            rng.gen_range(COLOR_MIN..=COLOR_MAX),
            rng.gen_range(COLOR_MIN..=COLOR_MAX),
            1.0,
        );
        material.metallic = rng.gen_range(0.0..=1.0);
        material.roughness = rng.gen_range(0.0..=1.0);
        material.reflectance = rng.gen_range(0.0..=1.0);
        commands.entity(entity).insert(AssignedMaterial);
    }
}
