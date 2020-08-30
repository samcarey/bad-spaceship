use bevy::prelude::*;

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system_to_stage("animate_characters", bob.system());
    }
}

const RADIANS_IN_CIRCLE: f32 = 2.0 * std::f32::consts::PI;

const SIZE: f32 = 1.5;
const HOVER_SIZE_RATIO: f32 = 0.2;

const BOB_RATIO: f32 = 0.15;
const BOB_RATE: f32 = 1.7;

struct Name(String);

struct Bob {
    amplitude: f32,
    phase: f32,
    radians_per_second: f32,
}

pub struct BasePosition(pub Vec3);

pub fn spawn(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> Option<Entity> {
    let hover = SIZE * HOVER_SIZE_RATIO;
    let bob_amplitude = hover * BOB_RATIO;
    let base_position = BasePosition(Vec3::new(0.0, SIZE * 1.5 + hover, 0.0));
    let cube_mat_handle = materials.add({
        let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
        cube_material.shaded = true;
        cube_material
    });
    commands
        .spawn(PbrComponents {
            mesh: meshes.add(Mesh::from(shape::Cube { size: SIZE })),
            material: cube_mat_handle,
            translation: Translation(base_position.0),
            ..Default::default()
        })
        .with(base_position)
        .with(Name("Name".to_string()))
        .with(Bob {
            amplitude: bob_amplitude,
            phase: 0.0,
            radians_per_second: BOB_RATE * RADIANS_IN_CIRCLE,
        })
        .current_entity()
}

fn bob(time: Res<Time>, mut query: Query<(&mut Translation, &mut Bob, &mut BasePosition)>) {
    for (mut translation, mut bob, mut base_position) in &mut query.iter() {
        bob.phase = (bob.phase + time.delta_seconds * bob.radians_per_second) % RADIANS_IN_CIRCLE;
        let bob_offset = Vec3::new(0.0, bob.amplitude * bob.phase.cos(), 0.0);
        translation.0 = base_position.0 + bob_offset;
    }
}
