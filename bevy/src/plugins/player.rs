use bevy::{
    input::mouse::{MouseMotion, MouseWheel},
    prelude::*,
};
use std::f32::consts::PI;

const DEG_TO_RADIANS: f32 = PI / 180.;

const MOVE_SPEED: f32 = 10.0;
const ZOOM_SENSITIVITY: f32 = 10.0;
const LOOK_SENSITIVITY: f32 = 1.0;

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
            .add_system(update_player.system());
    }
}

struct Player {
    yaw: f32,

    camera_distance: f32,
    camera_pitch: f32,
    camera_entity: Option<Entity>,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            yaw: 0.,

            camera_distance: 20.,
            camera_pitch: 30.0f32.to_radians(),
            camera_entity: None,
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
            camera_entity,
            camera_distance: 20.,
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
    mut query: Query<&mut Player>,
) {
    let mut look = Vec2::zero();
    for event in state.mouse_motion_event_reader.iter(&mouse_motion_events) {
        look = event.delta;
    }

    let mut zoom_delta = 0.;
    for event in state.mouse_wheel_event_reader.iter(&mouse_wheel_events) {
        zoom_delta = event.y;
    }

    for mut player in &mut query.iter() {
        player.yaw += look.x() * time.delta_seconds;
        player.camera_pitch = (player.camera_pitch
            - look.y() * time.delta_seconds * LOOK_SENSITIVITY)
            .min(MIN_CAMERA_PITCH)
            .max(MAX_CAMERA_PITCH);
        player.camera_distance = (player.camera_distance
            - zoom_delta * time.delta_seconds * ZOOM_SENSITIVITY)
            .min(MIN_CAMERA_DISTANCE)
            .max(MAX_CAMERA_DISTANCE);
    }
}

fn update_player(
    time: Res<Time>,
    keyboard_input: Res<Input<KeyCode>>,
    mut player_query: Query<(&mut Player, &mut Translation, &Transform, &mut Rotation)>,
    camera_query: Query<(&mut Translation, &mut Rotation)>,
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

    for (mut player, mut translation, transform, mut rotation) in &mut player_query.iter() {
        let fwd = transform.value.z_axis().truncate() * movement.y();
        let right = -transform.value.x_axis().truncate() * movement.x();

        translation.0 += Vec3::from(fwd + right);
        rotation.0 = Quat::from_rotation_y(-player.yaw);

        if let Some(camera_entity) = player.camera_entity {
            let cam_pos = Vec3::new(0., player.camera_pitch.cos(), -player.camera_pitch.sin())
                .normalize()
                * player.camera_distance;
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
