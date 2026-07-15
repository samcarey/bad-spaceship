//! Altitude-driven atmosphere: distance haze, ash density, and a star dome, all driven
//! by the one shared [`atmosphere_fraction`] so they fade in lockstep with the air (and
//! the drag physics that same fraction produces).
//!
//! - **Haze** — a [`DistanceFog`] on the main camera whose density tracks the air, so
//!   distant terrain washes into the sky low down and clears to crisp vacuum in space.
//! - **Ash** — the falling-flake field ([`AshMaterial`]) thins to nothing above the
//!   atmosphere (its shader culls flakes by the `density` we write here).
//! - **Stars** — a camera-anchored [`StarfieldMaterial`] dome that fades in as the haze
//!   and ash fade out (the inverse of the fraction), so the sky opens onto stars only
//!   once the air is thin.
//!
//! One system computes the camera's radial altitude once and drives all three, so they
//! can never disagree about "how high, how thick".

use bevy::{
    asset::uuid_handle,
    camera::visibility::NoFrustumCulling,
    mesh::MeshVertexBufferLayoutRef,
    pbr::{DistanceFog, FogFalloff, MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};
use bevy_egui::PrimaryEguiContext;

use bad_spaceship_shared::map::atmosphere_fraction;

use super::AshMaterial;

pub const STARFIELD_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("f2a7c9e4-1b60-4d83-9c2a-6e5b0d47a318");

/// The haze colour — a dusty, faintly-warm volcanic grey. Distant geometry fades toward
/// this, so it reads as aerial perspective against the ash sky (`ClearColor` in main.rs).
const FOG_COLOR: Color = Color::srgb(0.28, 0.19, 0.16);

/// Exponential fog density at the surface (per metre); scaled down by
/// [`atmosphere_fraction`] with altitude to zero in space. Tuned for aerial
/// perspective — the near platform stays crisp, half-haze lands around ~280 m, and the
/// far planet horizon (~775 m) reads as a hazed-but-present dusty limb rather than a
/// wall of fog.
const MAX_FOG_DENSITY: f32 = 0.0028;

/// A camera-anchored star dome. The single `Vec4` uniform carries star visibility in
/// `.x` (0 in thick air, 1 in clear space); the shader places every vertex on a
/// fixed-radius dome around the camera and paints a sparse twinkling field.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct StarfieldMaterial {
    #[uniform(0)]
    params: Vec4,
}

impl Material for StarfieldMaterial {
    fn vertex_shader() -> ShaderRef {
        STARFIELD_SHADER_HANDLE.into()
    }

    fn fragment_shader() -> ShaderRef {
        STARFIELD_SHADER_HANDLE.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    // A transparent, unlit, camera-relative dome writes no depth and casts no shadows —
    // and its custom vertex shader doesn't match the default prepass/shadow interface, so
    // opt both out (same reasoning as the ash field).
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    // The dome is viewed from the *inside*, so its outward-facing triangles are all
    // back-faces — disable culling or the whole sphere is invisible.
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// Spawn the one star-dome mesh (a unit sphere; the shader ignores its transform and
/// re-centres it on the camera each frame). `NoFrustumCulling` because the shader moves
/// the vertices far from the baked AABB.
fn spawn_starfield(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarfieldMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(1.0).mesh().ico(4).unwrap())),
        MeshMaterial3d(materials.add(StarfieldMaterial { params: Vec4::ZERO })),
        NoFrustumCulling,
        Name::new("Star dome"),
    ));
}

/// Drive haze, ash density, and star visibility from the main camera's radial altitude.
/// One `atmosphere_fraction` evaluation feeds all three so they stay consistent.
fn update_atmosphere(
    mut commands: Commands,
    // Present only in multiplayer (the floating-origin frame); single-player true == local.
    frame: Option<Res<crate::net::ClientRoomFrame>>,
    mut cameras: Query<
        (Entity, &GlobalTransform, Option<&mut DistanceFog>),
        (With<Camera3d>, With<PrimaryEguiContext>),
    >,
    mut ash: ResMut<Assets<AshMaterial>>,
    mut stars: ResMut<Assets<StarfieldMaterial>>,
) {
    let offset = frame.map(|f| f.offset.as_vec3()).unwrap_or(Vec3::ZERO);
    let Some((entity, transform, fog)) = cameras.iter_mut().next() else {
        return;
    };
    // The camera's TRUE world position (fold the room's floating-origin offset back in),
    // so altitude is real even under a rebase.
    let fraction = atmosphere_fraction(transform.translation() + offset);

    // Haze: exponential fog thinning to nothing in space.
    let density = MAX_FOG_DENSITY * fraction;
    match fog {
        Some(mut fog) => {
            fog.color = FOG_COLOR;
            fog.falloff = FogFalloff::Exponential { density };
        }
        None => {
            commands.entity(entity).try_insert(DistanceFog {
                color: FOG_COLOR,
                falloff: FogFalloff::Exponential { density },
                ..default()
            });
        }
    }

    // Ash thins with the air (its shader culls flakes by this fraction).
    for (_, material) in ash.iter_mut() {
        material.set_density(fraction);
    }

    // Stars are the inverse — they emerge as the air clears, eased so they brighten
    // gently rather than snapping on at the atmosphere edge.
    let t = (1.0 - fraction).clamp(0.0, 1.0);
    let visibility = t * t * (3.0 - 2.0 * t); // smoothstep
    for (_, material) in stars.iter_mut() {
        material.params.x = visibility;
    }
}

/// Register the star material, load its shader, and add the atmosphere systems.
pub fn plugin(app: &mut App) {
    bevy::asset::load_internal_asset!(
        app,
        STARFIELD_SHADER_HANDLE,
        "../../assets/starfield.wgsl",
        Shader::from_wgsl
    );
    app.add_plugins(MaterialPlugin::<StarfieldMaterial>::default())
        .add_systems(Startup, spawn_starfield)
        .add_systems(Update, update_atmosphere);
}
