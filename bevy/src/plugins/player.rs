use bevy::{
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
};
use std::f32::consts::PI;

const DEG_TO_RADIANS: f32 = PI / 180.;

const MOVE_SPEED: f32 = 10.;
const ZOOM_SENSITIVITY: f32 = 10.;
const LOOK_SENSITIVITY: f32 = 1.;

const MIN_CAMERA_DISTANCE: f32 = 5.;
const MAX_CAMERA_DISTANCE: f32 = 30.;
const MIN_CAMERA_PITCH_DEGREES: f32 = 1.;
const MAX_CAMERA_PITCH_DEGREES: f32 = 179.;

const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>()
            .add_startup_system(setup.system())
            .add_system(process_mouse_events.system())
            .add_system(process_keyboard_events.system())
            .add_system(update_player.system());
    }
}

struct Camera {
    distance: f32,
    pitch: f32,
    entity: Option<Entity>,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            distance: 20.,
            pitch: 30.0f32.to_radians(),
            entity: None,
        }
    }
}

struct Player {
    yaw: f32,
    camera: Camera,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            yaw: 0.,
            camera: Camera::default(),
        }
    }
}

#[derive(Default)]
struct State {
    mouse_motion_event_reader: EventReader<MouseMotion>,
    mouse_wheel_event_reader: EventReader<MouseWheel>,
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let cube_mat_handle = materials.add({
        let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
        cube_material.shaded = true;
        cube_material
    });

    // Spawn camera and player, set entity for camera on player.
    let camera_entity = commands
        .spawn(Camera3dComponents::default())
        .current_entity();

    let player_entity = commands
        .spawn(PbrComponents {
            mesh: meshes.add(Mesh::from(shape::Cube { size: 1.0 })),
            material: cube_mat_handle.clone(),
            translation: Translation::new(0.0, 1.0, 0.0),
            ..Default::default()
        })
        .with(Player {
            camera: Camera {
                entity: camera_entity,
                ..Default::default()
            },
            ..Default::default()
        })
        .current_entity();

    commands
        // Append camera to player as child.
        .push_children(player_entity.unwrap(), &[camera_entity.unwrap()]);
}

fn process_mouse_events(
    time: Res<Time>,
    mut state: ResMut<State>,
    mouse_motion_events: Res<Events<MouseMotion>>,
    mouse_wheel_events: Res<Events<MouseWheel>>,
    mut query: Query<(&mut Player, &mut Rotation)>,
) {
    let mut look = Vec2::zero();
    for event in state.mouse_motion_event_reader.iter(&mouse_motion_events) {
        look = event.delta;
    }

    let mut zoom_delta = 0.;
    for event in state.mouse_wheel_event_reader.iter(&mouse_wheel_events) {
        zoom_delta = event.y;
    }

    for (mut player, mut rotation) in &mut query.iter() {
        player.yaw += look.x() * time.delta_seconds;
        player.camera.pitch = (player.camera.pitch
            - look.y() * time.delta_seconds * LOOK_SENSITIVITY)
            .max(MIN_CAMERA_PITCH)
            .min(MAX_CAMERA_PITCH);
        player.camera.distance = (player.camera.distance
            - zoom_delta * time.delta_seconds * ZOOM_SENSITIVITY)
            .max(MIN_CAMERA_DISTANCE)
            .min(MAX_CAMERA_DISTANCE);
        rotation.0 = Quat::from_rotation_y(-player.yaw);
    }
}

fn process_keyboard_events(
    time: Res<Time>,
    keyboard_input: Res<Input<KeyCode>>,
    mut player_query: Query<(&mut Player, &mut Translation, &Transform)>,
) {
    let mut movement = Vec2::zero();
    if keyboard_input.pressed(KeyCode::W) {
        *movement.y_mut() += 1.;
    }
    if keyboard_input.pressed(KeyCode::S) {
        *movement.y_mut() -= 1.;
    }
    if keyboard_input.pressed(KeyCode::D) {
        *movement.x_mut() += 1.;
    }
    if keyboard_input.pressed(KeyCode::A) {
        *movement.x_mut() -= 1.;
    }

    if movement != Vec2::zero() {
        movement.normalize();
    }

    movement *= time.delta_seconds * MOVE_SPEED;

    for (_player, mut translation, transform) in &mut player_query.iter() {
        let fwd = transform.value.z_axis().truncate() * movement.y();
        let right = -transform.value.x_axis().truncate() * movement.x();
        translation.0 += Vec3::from(fwd + right);
    }
}

fn update_player(
    mut player_query: Query<&mut Player>,
    camera_query: Query<(&mut Translation, &mut Rotation)>,
) {
    for player in &mut player_query.iter() {
        if let Some(camera_entity) = player.camera.entity {
            let cam_pos = Vec3::new(0., player.camera.pitch.cos(), -player.camera.pitch.sin())
                .normalize()
                * player.camera.distance;
            if let Ok(mut cam_trans) = camera_query.get_mut::<Translation>(camera_entity) {
                cam_trans.0 = cam_pos;
            }

            if let Ok(mut camera_rotation) = camera_query.get_mut::<Rotation>(camera_entity) {
                let look = Mat4::face_toward(cam_pos, Vec3::zero(), Vec3::new(0.0, 1.0, 0.0));
                camera_rotation.0 = look.to_scale_rotation_translation().1;
            }
        }
    }
}
