cfg_if::cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        use bevy_webgl2::renderer::JsCast;
        use gloo::events::EventListener;
        use web_sys::MouseEvent;
        use std::sync::{
            atomic::{AtomicI32, Ordering::SeqCst},
            Arc,
        };
    } else {
        use bevy::input::mouse::MouseMotion;
    }
}

use super::super::{AppState, APP_STATE};
use bevy::{input::mouse::MouseWheel, prelude::*};
use serde::Deserialize;

use crate::plugins::character;

use std::f32::consts::PI;

const DEG_TO_RADIANS: f32 = PI / 180.;
const MIN_CAMERA_PITCH_DEGREES: f32 = 1.;
const MAX_CAMERA_PITCH_DEGREES: f32 = 179.;
const MIN_CAMERA_PITCH: f32 = MIN_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const MAX_CAMERA_PITCH: f32 = MAX_CAMERA_PITCH_DEGREES * DEG_TO_RADIANS;
const TWO_PI: f32 = std::f32::consts::PI * 2.0;

pub struct PlayerPlugin;
use character::CharacterPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<State>();
        #[cfg(target_arch = "wasm32")]
        app.add_resource(WasmMouseTracker::new());
        app.add_startup_system(setup.system())
            .on_state_update(APP_STATE, AppState::InGame, process_mouse_events.system())
            .on_state_update(
                APP_STATE,
                AppState::InGame,
                process_keyboard_events.system(),
            )
            .add_system(update_camera.system())
            .add_plugin(CharacterPlugin);
    }
}

#[derive(Deserialize, Copy, Clone)]
struct Config {
    zoom_sensitivity: f32,
    look_sensitivity: f32,

    min_camera_distance: f32,
    max_camera_distance: f32,
    camera_offset_character_size_ratio: (f32, f32, f32),
}

#[derive(Default)]
pub struct KeyboardDirectionalInput(pub Vec3);

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

impl Camera {
    fn new(camera_entity: Option<Entity>) -> Self {
        Camera {
            entity: camera_entity,
            ..Default::default()
        }
    }
}

#[derive(Default, Bundle)]
pub struct Player {
    camera: Camera,
}

pub struct Yaw(pub f32);

impl Player {
    fn new(camera_entity: Option<Entity>) -> Self {
        Player {
            camera: Camera::new(camera_entity),
        }
    }
}

#[derive(Default)]
struct State {
    #[cfg(not(target_arch = "wasm32"))]
    mouse_motion_event_reader: EventReader<MouseMotion>,
    mouse_wheel_event_reader: EventReader<MouseWheel>,
}

fn tuple_to_vec3(tuple: (f32, f32, f32)) -> Vec3 {
    let (x, y, z) = tuple;
    Vec3::new(x, y, z)
}

cfg_if::cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        struct WasmMouseTracker {
            delta_x: Arc<AtomicI32>,
            delta_y: Arc<AtomicI32>,
        }

        impl WasmMouseTracker {
            pub fn new() -> Self {
                let window = web_sys::window().expect("global window does not exists");
                let document = window.document().expect("expecting a document on window");
                let body = document
                    .body()
                    .expect("document expect to have have a body");

                let delta_x = Arc::new(AtomicI32::new(0));
                let delta_y = Arc::new(AtomicI32::new(0));

                let dx = Arc::clone(&delta_x);
                let dy = Arc::clone(&delta_y);
                let on_move = EventListener::new(&body, "mousemove", move |_event| {
                    let me = _event.clone().dyn_into::<MouseEvent>().unwrap();
                    dx.fetch_add(me.movement_x(), SeqCst);
                    dy.fetch_add(me.movement_y(), SeqCst);
                });
                on_move.forget();
                Self { delta_x, delta_y }
            }

            pub fn get_delta_and_reset(&self) -> Vec2 {
                let delta = Vec2::new(
                    self.delta_x.load(SeqCst) as f32,
                    self.delta_y.load(SeqCst) as f32,
                );
                self.delta_x.store(0, SeqCst);
                self.delta_y.store(0, SeqCst);
                delta
            }
        }
    }
}

fn setup(
    commands: &mut Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let config: Config = config_from_file!("player.ron");

    let camera_entity = commands.spawn(Camera3dBundle::default()).current_entity();

    let character_size = character::spawn(commands);

    let player_entity = commands
        .with(Player::new(camera_entity))
        .with(Yaw(0.))
        .with(config)
        .with(KeyboardDirectionalInput::default())
        .current_entity();

    // This is simply a point that hovers above the character that the camera orbits around.
    // This is for the purpose of making it easier to see over obstructions.
    // For now we generate this as a PbrComponent, which is overkill for an invisible point,
    // so we'll want to simplify this later to something with only the necessary components.
    let camera_center = commands
        .spawn(PbrBundle {
            mesh: meshes.add(Mesh::from(shape::Cube { size: 0.0 })),
            material: materials.add(StandardMaterial::default()),
            transform: Transform::from_translation(
                tuple_to_vec3(config.camera_offset_character_size_ratio) * character_size,
            ),
            ..Default::default()
        })
        .current_entity();

    // Mount the camera center to the player
    commands.push_children(player_entity.unwrap(), &[camera_center.unwrap()]);

    // Mount the camera to the camera center
    commands.push_children(camera_center.unwrap(), &[camera_entity.unwrap()]);
}

fn process_mouse_events(
    time: Res<Time>,
    mut state: ResMut<State>,
    #[cfg(not(target_arch = "wasm32"))] mouse_motion_events: Res<Events<MouseMotion>>,
    #[cfg(target_arch = "wasm32")] wasm_mouse_tracker: Res<WasmMouseTracker>,
    mouse_wheel_events: Res<Events<MouseWheel>>,
    mut query: Query<(&mut Player, &Config, &mut Yaw)>,
) {
    cfg_if::cfg_if! {
        if #[cfg(not(target_arch = "wasm32"))] {
            let mut look = Vec2::zero();
            for event in state.mouse_motion_event_reader.iter(&mouse_motion_events) {
                look = event.delta;
            }
        } else {
            let look = wasm_mouse_tracker.get_delta_and_reset();
        }
    }

    let mut zoom_delta = 0.;
    for event in state.mouse_wheel_event_reader.iter(&mouse_wheel_events) {
        zoom_delta = event.y;
    }

    for (mut player, config, mut yaw) in query.iter_mut() {
        yaw.0 = (yaw.0 + look.x * time.delta_seconds() * config.look_sensitivity) % TWO_PI;
        player.camera.pitch = (player.camera.pitch
            - look.y * time.delta_seconds() * config.look_sensitivity)
            .max(MIN_CAMERA_PITCH)
            .min(MAX_CAMERA_PITCH);
        player.camera.distance = (player.camera.distance
            - zoom_delta * time.delta_seconds() * config.zoom_sensitivity)
            .max(config.min_camera_distance)
            .min(config.max_camera_distance);
    }
}

fn process_keyboard_events(
    keyboard_input: Res<Input<KeyCode>>,
    mut query: Query<Mut<KeyboardDirectionalInput>>,
) {
    for mut keyboard_directional_input in query.iter_mut() {
        //
        // Note: keyboard_directional_input vector components match Bevy/Rapier vector definitions:
        //  Horizontal = (X,Z)
        //  Vertical = Y
        //

        // Initialize to zero every time - if a key is pressed then it will overwrite in the section below.
        keyboard_directional_input.0 = Vec3::zero();

        // "W" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::W) {
            keyboard_directional_input.0.z += 1.;
        }

        // "S" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::S) {
            keyboard_directional_input.0.z -= 1.;
        }

        // "D" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::D) {
            keyboard_directional_input.0.x += 1.;
        }

        // "A" keypress indicates forward movement
        if keyboard_input.pressed(KeyCode::A) {
            keyboard_directional_input.0.x -= 1.;
        }

        //
        // "Spacebar" keypress indicates vertical jump / thrust.
        //
        //  TODO:   We need to control directional input here to isolate jump event vs. continuous
        //          upward thrust.
        //
        if keyboard_input.pressed(KeyCode::Space) {
            keyboard_directional_input.0.y += 1.;
        }

        // Check here to see if any keypresses were registered.
        // If so, then normalize the vector components.
        if keyboard_directional_input.0 != Vec3::zero() {
            keyboard_directional_input.0.normalize();
        }
    }
}

fn update_camera(mut player_query: Query<&mut Player>, mut camera_query: Query<&mut Transform>) {
    for player in player_query.iter_mut() {
        if let Some(camera_entity) = player.camera.entity {
            let cam_pos = Vec3::new(0., player.camera.pitch.cos(), -player.camera.pitch.sin())
                .normalize()
                * player.camera.distance;
            if let Ok(mut transform) = camera_query.get_mut(camera_entity) {
                transform.translation = cam_pos;

                let look = Mat4::face_toward(cam_pos, Vec3::zero(), Vec3::new(0.0, 1.0, 0.0));
                transform.rotation = look.to_scale_rotation_translation().1;
            }
        }
    }
}
