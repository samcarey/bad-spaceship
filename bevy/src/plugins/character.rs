use bevy::prelude::*;

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(add_character.system())
            .add_system(move_character.system());
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

struct BasePosition(Vec3);

pub fn get_model(
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) -> PbrComponents {
    let cube_mat_handle = materials.add({
        let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
        cube_material.shaded = true;
        cube_material
    });
    PbrComponents {
        mesh: meshes.add(Mesh::from(shape::Cube { size: SIZE })),
        material: cube_mat_handle,
        translation: Translation::new(0.0, 1.0, 0.0),
        ..Default::default()
    }
}

// impl Character {
//     pub fn new(
//         mut meshes: ResMut<Assets<Mesh>>,
//         mut materials: ResMut<Assets<StandardMaterial>>,
//     ) -> Self {
//         let cube_mat_handle = materials.add({
//             let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
//             cube_material.shaded = true;
//             cube_material
//         });
//         let model = PbrComponents {
//             mesh: meshes.add(Mesh::from(shape::Cube { size: 1.0 })),
//             material: cube_mat_handle.clone(),
//             translation: Translation::new(0.0, 1.0, 0.0),
//             ..Default::default()
//         };
//         Character {
//             model: Some(model),
//             ..Default::default()
//         }
//     }
// }

fn add_character(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let hover = SIZE * HOVER_SIZE_RATIO;
    let bob_amplitude = hover * BOB_RATIO;
    let base_position = BasePosition(Vec3::new(0.0, SIZE * 1.5 + hover, 0.0));
    commands
        .spawn(PbrComponents {
            mesh: meshes.add(Mesh::from(shape::Cube { size: SIZE })),
            material: materials.add(Color::rgb(0.5, 0.4, 0.3).into()),
            translation: Translation(base_position.0),
            ..Default::default()
        })
        .with(base_position)
        .with(Name("Name".to_string()))
        .with(Bob {
            amplitude: bob_amplitude,
            phase: 0.0,
            radians_per_second: BOB_RATE * RADIANS_IN_CIRCLE,
        });
}

fn move_character(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut query: Query<(
        &Name,
        &mut Translation,
        &Handle<StandardMaterial>,
        &mut Bob,
        &mut BasePosition,
    )>,
) {
    for (_name, mut translation, material_handle, mut bob, mut base_position) in &mut query.iter() {
        let material = materials.get_mut(&material_handle).unwrap();
        material.albedo =
            Color::BLUE * Vec3::splat((3.0 * time.seconds_since_startup as f32).sin());

        base_position.0 += Vec3::new(1.0, 0.0, 0.0) * time.delta_seconds;
        bob.phase = (bob.phase + time.delta_seconds * bob.radians_per_second) % RADIANS_IN_CIRCLE;
        let bob_offset = Vec3::new(0.0, bob.amplitude * bob.phase.cos(), 0.0);
        translation.0 = base_position.0 + bob_offset;
    }
}
