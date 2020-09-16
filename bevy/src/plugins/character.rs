use bevy::prelude::*;
use bevy_rapier3d::physics::RigidBodyHandleComponent;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use rapier3d::dynamics::RigidBodySet;
use rapier3d::math;
use rapier3d::math::{Isometry, Matrix, Vector};
use serde::Deserialize;

use crate::plugins::player;
pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app
            // .add_system(bob.system())
            .add_system_to_stage(stage::LAST, propel.system());
    }
}

const CONFIG_FILE: &str = "assets/config/character.ron";

const RADIANS_IN_CIRCLE: f32 = 2.0 * std::f32::consts::PI;

struct Name(String);
struct BasePosition(pub Vec3);
pub struct Propulsion {
    max_force: f32,
    max_speed: f32,
}

struct Bob {
    amplitude: f32,
    phase: f32,
    radians_per_second: f32,
}

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    size: f32,
    hover_size_ratio: f32,
    bob_ratio: f32,
    bob_rate: f32,
    extra_first_person_height_size_ratio: f32,
    name: String,
    max_speed: f32,
    max_propulsion: f32,
}

pub fn spawn(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let config = Config::new(CONFIG_FILE);

    let hover = config.size * config.hover_size_ratio;
    let base_height = config.size / 2.0 + hover;
    let static_height_of_top = config.size + hover;
    let bob_amplitude = hover * config.bob_ratio;
    let dynamic_height_of_top = static_height_of_top + bob_amplitude;
    let extra_first_person_height = config.extra_first_person_height_size_ratio * config.size;
    let first_person_camera_height = dynamic_height_of_top * extra_first_person_height;

    let base_position = BasePosition(Vec3::new(
        0.0,
        base_height * 55. - first_person_camera_height,
        0.0,
    ));

    // let cube_mat_handle = materials.add({
    //     let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
    //     cube_material.shaded = true;
    //     cube_material
    // });

    let rigid_body = RigidBodyBuilder::new_dynamic().translation(0.0, 50.0, 0.0);
    let collider = ColliderBuilder::cuboid(config.size / 2.0, config.size / 2.0, config.size / 2.0)
        .density(1.0)
        .friction(1.0);

    let character_entity = commands
        // .spawn(PbrComponents {
        //     mesh: meshes.add(Mesh::from(shape::Cube {
        //         size: config.size / 2.0,
        //     })),
        //     material: cube_mat_handle,
        //     translation: Translation(base_position.0),
        //     ..Default::default()
        // })
        .spawn((rigid_body, collider))
        .with(base_position)
        .with(Propulsion {
            max_speed: config.max_speed,
            max_force: config.max_propulsion,
        })
        .with(Name(config.name))
        .with(Bob {
            amplitude: bob_amplitude,
            phase: 0.0,
            radians_per_second: config.bob_rate * RADIANS_IN_CIRCLE,
        });
    // .current_entity();
    // (character_entity, first_person_camera_height)
}

fn vec3_to_vector(v: Vec3) -> Vector<f32> {
    Vector::new(v.x(), v.y(), v.z())
}

fn vector_to_vec3(v: Vector<f32>) -> Vec3 {
    Vec3::new(v[(0, 0)], v[(1, 1)], v[(0, 2)])
}

fn propel(
    mut bodies: ResMut<RigidBodySet>,
    keyboard_directional_input: &player::KeyboardDirectionalInput,
    rigid_body: &RigidBodyHandleComponent,
    transform: &Transform,
    propulsion: &Propulsion,
) {
    if let Some(mut rb) = bodies.get_mut(rigid_body.handle()) {
        let forward = transform.value.z_axis().truncate() * keyboard_directional_input.0.y();
        let right = -transform.value.x_axis().truncate() * keyboard_directional_input.0.x();
        let propulsion_unit_vector = vec3_to_vector(Vec3::from(forward + right));

        if propulsion_unit_vector.amax() > 0.0 {
            rb.wake_up();

            // let new_translation_vector = Vector::new(

            // );

            // let new_translation = math::Translation::new(
            //     rb.position.translation.vector.x + 3.0,
            //     rb.position.translation.vector.y,
            //     rb.position.translation.vector.z,
            // );

            let new_translation = rb.position.translation.vector + propulsion_unit_vector;

            let position = Isometry::new(new_translation, rb.position.rotation.as_vector());

            rb.set_position(position);
            // let mut propulsive_force = propulsion_unit_vector * propulsion.max_force;
            // let current_speed_along_propulsion_direction = rb.linvel.dot(&propulsion_unit_vector);
            // let relative_remaining_propulsion = (propulsion.max_speed
            //     - current_speed_along_propulsion_direction)
            //     / propulsion.max_speed;

            // if relative_remaining_propulsion < 1.0 {
            //     propulsive_force *= relative_remaining_propulsion;
            //     println!("{}", relative_remaining_propulsion);
            // }
            // rb.apply_force(propulsive_force);
        }
    }
}

fn bob(time: Res<Time>, mut query: Query<(&mut Translation, &mut Bob, &mut BasePosition)>) {
    for (mut translation, mut bob, base_position) in &mut query.iter() {
        bob.phase = (bob.phase + time.delta_seconds * bob.radians_per_second) % RADIANS_IN_CIRCLE;
        let bob_offset = Vec3::new(0.0, bob.amplitude * bob.phase.cos(), 0.0);
        translation.0 = base_position.0 + bob_offset;
    }
}
