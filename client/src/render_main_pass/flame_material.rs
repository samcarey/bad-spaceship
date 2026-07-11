//! Rocket-exhaust flames: an additive, unlit, fully procedural WGSL plume on
//! every thrusting rocket (`client/assets/flame_material.wgsl` shapes the mesh
//! in the vertex stage and paints it in the fragment stage). This module owns
//! the material, the per-rocket flame state, and the per-frame driver system;
//! the flame child itself is spawned by `insert_rocket_visual` so single-player
//! and multiplayer rockets get it from the same constructor.
//!
//! Data flow per rocket:
//! - the thrust systems (`launch.rs`) write the tick's commanded throttle into
//!   [`FlameThrottle::target`] (reset to 0 every tick first, so a rocket that
//!   leaves the burning assembly — or a room that never launched — reads 0);
//! - [`update_flames`] eases a displayed strength toward that target (fast
//!   attack, slower decay, so ignition snaps and cutoff lingers), orients the
//!   flame along the *gimballed* exhaust direction, raycasts the exhaust axis
//!   against the ground for the splash deflection, and writes the material.

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bad_spaceship_shared::launch::gimbal_thrust_dir_local;
use bad_spaceship_shared::part::{Gimbal, RocketEngine, ROCKET_FLARE_HEIGHT, ROCKET_FLARE_Y_OFFSET};
use bad_spaceship_shared::Grass;
use bevy::{
    asset::uuid_handle,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

/// The procedural flame shader (`client/assets/flame_material.wgsl`), embedded
/// at compile time by `RenderMainPassPlugin` like the other material shaders.
pub const FLAME_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("7f1a34c2-90de-4b6a-a26c-4816f31cf1b7");

/// The one shared flame mesh (every flame is the same unit cylinder — the shader
/// does all the shaping), registered under a fixed handle by `register_flame_mesh`
/// so `spawn_flame` never allocates per-rocket copies.
pub const FLAME_MESH_HANDLE: Handle<Mesh> =
    uuid_handle!("3e0f5a81-2b77-49c4-9b53-24c1d0a1b9ce");

/// Insert the shared unit-cylinder flame mesh (plenty of height segments — the
/// ground-splash bend happens per-vertex). Called once from the plugin build.
pub fn register_flame_mesh(meshes: &mut Assets<Mesh>) {
    meshes.insert(
        FLAME_MESH_HANDLE.id(),
        // Enough rings for the ground-splash bend AND the ragged silhouette
        // ripple to read smoothly over the (long) plume.
        Cylinder::new(1.0, 1.0).mesh().resolution(32).segments(32).into(),
    );
}

/// Flame-local distance from the nozzle to the plume tip at full throttle.
const FLAME_LENGTH: f32 = 9.6;

/// `ground_dist` sentinel for "no ground within reach" (any value far past the
/// plume's length disables the splash branch in the vertex shader).
const NO_GROUND: f32 = 1.0e6;

/// Displayed-strength easing rates (per second): ignition snaps on, cutoff
/// fades out like a real engine spooling down.
const ATTACK_RATE: f32 = 10.0;
const DECAY_RATE: f32 = 3.5;

/// Peak luminous power (lumens) of a rocket's exhaust light at full throttle,
/// scaled by displayed strength each frame. Big enough to visibly wash nearby
/// parts and the pad in ember light against the faint ash-overcast key light.
const FLAME_LIGHT_INTENSITY: f32 = 6_000_000.0;
/// How far the exhaust light reaches (metres) — a bit past the full plume so the
/// ground splash lights the terrain around the pad before it falls off.
const FLAME_LIGHT_RANGE: f32 = FLAME_LENGTH * 2.5;
/// Warm ember tint of the exhaust light.
const FLAME_LIGHT_COLOR: Color = Color::srgb(1.0, 0.55, 0.16);

/// Mirrors `FlameParams` in the WGSL field-for-field.
#[derive(ShaderType, Debug, Clone)]
pub struct FlameParams {
    strength: f32,
    ground_dist: f32,
    phase: f32,
    flame_len: f32,
    ground_normal: Vec4,
}

impl Default for FlameParams {
    fn default() -> Self {
        Self {
            strength: 0.0,
            ground_dist: NO_GROUND,
            phase: 0.0,
            flame_len: FLAME_LENGTH,
            ground_normal: Vec4::Y,
        }
    }
}

#[derive(Asset, AsBindGroup, Debug, Clone, Default, TypePath)]
pub struct FlameMaterial {
    #[uniform(0)]
    params: FlameParams,
}

impl Material for FlameMaterial {
    fn vertex_shader() -> ShaderRef {
        FLAME_SHADER_HANDLE.into()
    }

    fn fragment_shader() -> ShaderRef {
        FLAME_SHADER_HANDLE.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        // Additive: overlap glows, draw order between flames doesn't matter,
        // and the transparent pass skips depth writes for us.
        AlphaMode::Add
    }
}

/// Per-rocket flame drive. `target` is the authoritative per-tick throttle the
/// thrust systems write (zeroed at the top of every physics tick); `eased` is
/// the displayed strength `update_flames` animates toward it.
#[derive(Component, Default)]
pub struct FlameThrottle {
    pub target: f32,
    pub eased: f32,
}

/// Points a rocket at its flame child (spawned by `insert_rocket_visual`).
#[derive(Component)]
pub struct FlameOf(pub Entity);

/// Points a rocket at its flame's real `PointLight` child — the one that lights
/// up the surrounding terrain and parts (`update_flames` slides it down the
/// exhaust to the plume tip or the ground splash).
#[derive(Component)]
pub struct FlameLightOf(pub Entity);

/// Marks the exhaust `PointLight` so `update_flames` can hold two disjoint
/// `&mut Transform` queries (the flame mesh vs. its light) without aliasing.
#[derive(Component)]
pub struct FlameLight;

/// Build the flame child for a rocket: the shared unit cylinder (the shader does
/// all the shaping) hung at the flare exit, hidden until the rocket actually burns.
pub fn spawn_flame(entity: &mut EntityCommands, flame_materials: &mut Assets<FlameMaterial>) {
    // Desync neighbouring flames' flicker with a per-entity phase.
    let phase = (entity.id().to_bits() % 251) as f32 * 0.417;
    let material = flame_materials.add(FlameMaterial {
        params: FlameParams { phase, ..Default::default() },
    });
    let mut flame = Entity::PLACEHOLDER;
    let mut light = Entity::PLACEHOLDER;
    entity.with_children(|parent| {
        flame = parent
            .spawn((
                Mesh3d(FLAME_MESH_HANDLE.clone()),
                MeshMaterial3d(material),
                // The flare's exit plane (its bottom face).
                Transform::from_xyz(0.0, ROCKET_FLARE_Y_OFFSET - ROCKET_FLARE_HEIGHT / 2.0, 0.0),
                Visibility::Hidden,
                bevy::light::NotShadowCaster,
            ))
            .with_children(|flame| {
                // A real point light rides *inside* the plume so the fire actually
                // illuminates nearby parts and the terrain. It's a child of the
                // flame, which points down the (gimballed) exhaust axis — so a
                // local `-Y` offset places it along the real beam, and when the
                // exhaust hits the ground `update_flames` drops it onto the splash
                // point (not a fixed source at the nozzle). Hidden with the flame
                // (inherited visibility) and its intensity zeroed when not burning.
                light = flame
                    .spawn((
                        FlameLight,
                        bevy::light::PointLight {
                            color: FLAME_LIGHT_COLOR,
                            intensity: 0.0,
                            range: FLAME_LIGHT_RANGE,
                            radius: 0.4,
                            shadow_maps_enabled: false,
                            ..default()
                        },
                        Transform::from_xyz(0.0, -FLAME_LENGTH * 0.5, 0.0),
                    ))
                    .id();
            })
            .id();
    });
    entity.insert((FlameThrottle::default(), FlameOf(flame), FlameLightOf(light)));
}

/// Animate every rocket's flame from its per-tick throttle: ease the displayed
/// strength, aim the plume along the gimballed exhaust, raycast the exhaust
/// axis against the ground (splash deflection), and push it all into the
/// material. Runs in both modes — the flame child rides `insert_rocket_visual`.
#[allow(clippy::type_complexity)]
pub fn update_flames(
    time: Res<Time>,
    spatial: SpatialQuery,
    grass: Query<(), With<Grass>>,
    mut rockets: Query<
        (&GlobalTransform, &Gimbal, &mut FlameThrottle, &FlameOf, &FlameLightOf),
        With<RocketEngine>,
    >,
    mut flames: Query<
        (&mut Transform, &mut Visibility, &MeshMaterial3d<FlameMaterial>),
        Without<FlameLight>,
    >,
    mut flame_lights: Query<(&mut Transform, &mut bevy::light::PointLight), With<FlameLight>>,
    mut materials: ResMut<Assets<FlameMaterial>>,
) {
    let dt = time.delta_secs();
    for (global, gimbal, mut throttle, flame_of, flame_light_of) in &mut rockets {
        let rate = if throttle.target > throttle.eased { ATTACK_RATE } else { DECAY_RATE };
        let target = throttle.target;
        throttle.eased += (target - throttle.eased) * (1.0 - (-rate * dt).exp());

        let Ok((mut transform, mut visibility, material)) = flames.get_mut(flame_of.0) else {
            continue;
        };
        if throttle.eased < 0.02 {
            *visibility = Visibility::Hidden;
            if let Ok((_, mut light)) = flame_lights.get_mut(flame_light_of.0) {
                // Guard the write: most rockets are idle most of the time, and an
                // unconditional store would dirty every idle rocket's PointLight
                // (→ render-world re-extraction) every frame. 0.0 is exact here.
                if light.intensity != 0.0 {
                    light.intensity = 0.0;
                }
            }
            continue;
        }
        *visibility = Visibility::Inherited;

        // Exhaust = the opposite of the gimballed thrust direction (shared tilt law).
        let exhaust_local = -gimbal_thrust_dir_local(gimbal.0).normalize_or_zero();
        let flame_rotation = Quat::from_rotation_arc(Vec3::NEG_Y, exhaust_local);
        transform.rotation = flame_rotation;

        // Splash raycast: from the flare exit along the world exhaust axis,
        // ground (Grass) only — parts and avatars don't deflect the plume.
        let (_, rocket_rotation, _) = global.to_scale_rotation_translation();
        let origin = global.transform_point(transform.translation);
        let exhaust_world = rocket_rotation * exhaust_local;
        let mut ground_dist = NO_GROUND;
        let mut ground_normal = Vec3::Y;
        if let Ok(dir) = Dir3::new(exhaust_world) {
            if let Some(hit) = spatial.cast_ray_predicate(
                origin,
                dir,
                FLAME_LENGTH * 1.5,
                true,
                &SpatialQueryFilter::default(),
                &|entity| grass.contains(entity),
            ) {
                ground_dist = hit.distance;
                // Into flame-local space (the shader bends in local coords).
                ground_normal = (rocket_rotation * flame_rotation).inverse() * hit.normal;
            }
        }

        // Slide the exhaust light to where the plume actually ends, and drive its
        // brightness off the displayed strength. If the exhaust hits the ground
        // within reach, sit the light on the splash point so the fanned-out fire
        // lights the terrain there; otherwise drop it partway down the free plume.
        // The light is a child of the flame (which points down the exhaust), so a
        // local `-Y` offset already tracks the gimballed, ground-bent beam. Writes
        // are change-gated (same epsilon idea as the material upload below) so a
        // converged burn stops re-extracting the light every frame; each rocket
        // owns one clustered PointLight, so keep the count/range modest on WebGL2.
        if let Ok((mut light_tf, mut light)) = flame_lights.get_mut(flame_light_of.0) {
            let reach = (FLAME_LENGTH * throttle.eased).max(0.5);
            let d = if ground_dist < NO_GROUND { ground_dist.min(reach) } else { reach * 0.5 };
            let intensity = throttle.eased * FLAME_LIGHT_INTENSITY;
            if (light_tf.translation.y + d).abs() > 1.0e-3 {
                light_tf.translation.y = -d;
            }
            if (light.intensity - intensity).abs() > FLAME_LIGHT_INTENSITY * 2.0e-3 {
                light.intensity = intensity;
            }
        }

        // Mutating the asset re-uploads the uniform — gate on real change so a
        // converged burn (steady throttle, ground out of reach) goes GPU-quiet;
        // the flicker itself animates off `globals.time`, not these params.
        let Some(mat) = materials.get(material.id()) else {
            continue;
        };
        if (mat.params.strength - throttle.eased).abs() > 2.0e-3
            || (mat.params.ground_dist - ground_dist).abs() > 1.0e-2
            || mat.params.ground_normal.truncate().distance_squared(ground_normal) > 1.0e-4
        {
            if let Some(mut mat) = materials.get_mut(material.id()) {
                mat.params.strength = throttle.eased;
                mat.params.ground_dist = ground_dist;
                mat.params.ground_normal = ground_normal.extend(0.0);
            }
        }
    }
}
