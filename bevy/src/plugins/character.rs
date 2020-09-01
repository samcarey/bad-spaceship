use bevy::prelude::*;
use serde::Deserialize;
use std::fs;

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(bob.system());
    }
}

const CONFIG_FILE: &str = "assets/config/character.ron";

const RADIANS_IN_CIRCLE: f32 = 2.0 * std::f32::consts::PI;

struct Name(String);

struct Bob {
    amplitude: f32,
    phase: f32,
    radians_per_second: f32,
}

pub struct BasePosition(pub Vec3);

#[derive(Deserialize)]
struct Config {
    size: f32,
    hover_size_ratio: f32,
    bob_ratio: f32,
    bob_rate: f32,
    extra_first_person_height_size_ratio: f32,
    name: String,
}

impl Config {
    fn new() -> Self {
        let config_string = fs::read_to_string(CONFIG_FILE).unwrap();
        ron::from_str(&config_string[..]).unwrap()
    }
}

pub fn spawn(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> (Option<Entity>, f32) {
    let config = Config::new();

    let hover = config.size * config.hover_size_ratio;
    let base_height = config.size / 2.0 + hover;
    let static_height_of_top = config.size + hover;
    let bob_amplitude = hover * config.bob_ratio;
    let dynamic_height_of_top = static_height_of_top + bob_amplitude;
    let extra_first_person_height = config.extra_first_person_height_size_ratio * config.size;
    let first_person_camera_height = dynamic_height_of_top * extra_first_person_height;

    let base_position = BasePosition(Vec3::new(
        0.0,
        base_height - first_person_camera_height,
        0.0,
    ));
    let cube_mat_handle = materials.add({
        let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
        cube_material.shaded = true;
        cube_material
    });
    let character_entity = commands
        .spawn(PbrComponents {
            mesh: meshes.add(Mesh::from(shape::Cube {
                size: config.size / 2.0,
            })),
            material: cube_mat_handle,
            translation: Translation(base_position.0),
            ..Default::default()
        })
        .with(base_position)
        .with(Name(config.name))
        .with(Bob {
            amplitude: bob_amplitude,
            phase: 0.0,
            radians_per_second: config.bob_rate * RADIANS_IN_CIRCLE,
        })
        .current_entity();
    (character_entity, first_person_camera_height)
}

fn bob(time: Res<Time>, mut query: Query<(&mut Translation, &mut Bob, &mut BasePosition)>) {
    for (mut translation, mut bob, base_position) in &mut query.iter() {
        bob.phase = (bob.phase + time.delta_seconds * bob.radians_per_second) % RADIANS_IN_CIRCLE;
        let bob_offset = Vec3::new(0.0, bob.amplitude * bob.phase.cos(), 0.0);
        translation.0 = base_position.0 + bob_offset;
    }
}
