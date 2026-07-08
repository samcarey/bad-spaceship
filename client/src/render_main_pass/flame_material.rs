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
use bad_spaceship_shared::part::{
    Gimbal, RocketEngine, ROCKET_FLARE_HEIGHT, ROCKET_FLARE_Y_OFFSET, ROCKET_THRUST_DIR_LOCAL,
};
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

/// Flame-local distance from the nozzle to the plume tip at full throttle.
const FLAME_LENGTH: f32 = 3.2;

/// `ground_dist` sentinel for "no ground within reach" (any value far past the
/// plume's length disables the splash branch in the vertex shader).
const NO_GROUND: f32 = 1.0e6;

/// Displayed-strength easing rates (per second): ignition snaps on, cutoff
/// fades out like a real engine spooling down.
const ATTACK_RATE: f32 = 10.0;
const DECAY_RATE: f32 = 3.5;

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

/// Build the flame child for a rocket: a unit cylinder (the shader does all the
/// shaping) hung at the flare exit, hidden until the rocket actually burns.
/// Returns the components for the rocket entity itself.
pub fn spawn_flame(
    entity: &mut EntityCommands,
    meshes: &mut Assets<Mesh>,
    flame_materials: &mut Assets<FlameMaterial>,
) {
    // Plenty of height segments — the ground-splash bend happens per-vertex.
    let mesh = meshes.add(Cylinder::new(1.0, 1.0).mesh().resolution(28).segments(20));
    // Desync neighbouring flames' flicker with a per-entity phase.
    let phase = (entity.id().to_bits() % 251) as f32 * 0.417;
    let material = flame_materials.add(FlameMaterial {
        params: FlameParams { phase, ..Default::default() },
    });
    let mut flame = Entity::PLACEHOLDER;
    entity.with_children(|parent| {
        flame = parent
            .spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material),
                // The flare's exit plane (its bottom face).
                Transform::from_xyz(0.0, ROCKET_FLARE_Y_OFFSET - ROCKET_FLARE_HEIGHT / 2.0, 0.0),
                Visibility::Hidden,
                bevy::light::NotShadowCaster,
            ))
            .id();
    });
    entity.insert((FlameThrottle::default(), FlameOf(flame)));
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
        (&GlobalTransform, &Gimbal, &mut FlameThrottle, &FlameOf),
        With<RocketEngine>,
    >,
    mut flames: Query<(&mut Transform, &mut Visibility, &MeshMaterial3d<FlameMaterial>)>,
    mut materials: ResMut<Assets<FlameMaterial>>,
) {
    let dt = time.delta_secs();
    for (global, gimbal, mut throttle, flame_of) in &mut rockets {
        let rate = if throttle.target > throttle.eased { ATTACK_RATE } else { DECAY_RATE };
        let target = throttle.target;
        throttle.eased += (target - throttle.eased) * (1.0 - (-rate * dt).exp());

        let Ok((mut transform, mut visibility, material)) = flames.get_mut(flame_of.0) else {
            continue;
        };
        if throttle.eased < 0.02 {
            *visibility = Visibility::Hidden;
            continue;
        }
        *visibility = Visibility::Inherited;

        // The gimballed thrust direction in rocket-local space (the same tilt
        // `gimbaled_rocket_thrust` applies); exhaust is its opposite.
        let angle = gimbal.0.length();
        let thrust_local = if angle < 1e-6 {
            ROCKET_THRUST_DIR_LOCAL
        } else {
            ROCKET_THRUST_DIR_LOCAL * angle.cos()
                + Vec3::new(gimbal.0.x, 0.0, gimbal.0.y) / angle * angle.sin()
        };
        let exhaust_local = -thrust_local.normalize_or_zero();
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

        if let Some(mut mat) = materials.get_mut(material.id()) {
            mat.params.strength = throttle.eased;
            mat.params.ground_dist = ground_dist;
            mat.params.ground_normal = ground_normal.extend(0.0);
        }
    }
}
