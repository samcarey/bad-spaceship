use bevy::prelude::*;

pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app
            // .add_startup_system(add_camera.system())
            .add_startup_system(add_lighting.system())
            .add_startup_system(add_platform.system());
    }
}

const PLATFORM_SIZE: f32 = 15.0;

fn add_lighting(mut commands: Commands) {
    commands.spawn(LightComponents {
        translation: Translation::new(4.0, 8.0, 4.0),
        ..Default::default()
    });
}

struct Platform(PbrComponents);

fn add_platform(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn(PbrComponents {
        mesh: meshes.add(Mesh::from(shape::Plane {
            size: PLATFORM_SIZE,
        })),
        material: materials.add(Color::rgb(0.1, 0.2, 0.1).into()),
        ..Default::default()
    });
}
