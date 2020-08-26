use bevy::prelude::*;

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(add_character.system())
            .add_system(move_character.system());
    }
}

const SIZE: f32 = 1.5;

struct Name(String);

fn add_character(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands
        .spawn(PbrComponents {
            mesh: meshes.add(Mesh::from(shape::Cube { size: SIZE })),
            material: materials.add(Color::rgb(0.5, 0.4, 0.3).into()),
            translation: Translation::new(0.0, 1.0, 0.0),
            ..Default::default()
        })
        .with(Name("Name".to_string()));
}

fn move_character(
    time: Res<Time>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut query: Query<(&Name, &mut Translation, &Handle<StandardMaterial>)>,
) {
    for (_name, mut translation, material_handle) in &mut query.iter() {
        let material = materials.get_mut(&material_handle).unwrap();
        translation.0 += Vec3::new(1.0, 0.0, 0.0) * time.delta_seconds;
        material.albedo =
            Color::BLUE * Vec3::splat((3.0 * time.seconds_since_startup as f32).sin());
    }
}
