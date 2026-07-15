//! The atmosphere, physically based, from one model at every altitude.
//!
//! The planet wears an exponential shell of lava-lit smog (`map::atmosphere_fraction`
//! — the same profile the drag physics flies through). What any pixel sees is the
//! optical depth of its sightline through that air, `τ = extinction·∫ρ dl`, computed by
//! the shared WGSL library `bad_spaceship::atmosphere` (`atmosphere_fog.wgsl`):
//!
//! - **The sky** is a dome (`sky.wgsl`) shading every direction as
//!   `smog·(1−T) + stars·T` along the infinite ray. The ground-level smog wall, stars
//!   piercing the zenith first as you climb, the limb ring + halo from orbit, and the
//!   planet occluding the stars behind it all fall out of the one integral — there is
//!   no boundary to cross and nothing to fade in or out.
//! - **The planet** (magma — the scene's only long-sightline geometry) attenuates its
//!   lit color by the exact camera→fragment transmittance in its own shader.
//! - **Near-field meshes** (parts, monsters, grass — always within a few hundred
//!   metres) use Bevy's [`DistanceFog`] with density = extinction·ρ(camera): the exact
//!   integral's short-path limit, so they blend seamlessly with the exact-fogged far
//!   field.
//! - **Ash flakes** thin with the local air (their shader culls by `density`), gone
//!   above the atmosphere.

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

use bad_spaceship_shared::map::atmosphere_optical_fraction;

use super::AshMaterial;

pub const ATMOSPHERE_FOG_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("8d5b2e91-4c7a-4f06-b3e8-a1d92c60f574");

pub const SKY_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("f2a7c9e4-1b60-4d83-9c2a-6e5b0d47a318");

/// Extinction coefficient at surface air density (m⁻¹) — THE atmosphere opacity knob.
/// Mirrors `EXTINCTION` in `atmosphere_fog.wgsl` (the exact-integral shaders); here it
/// drives the near-field [`DistanceFog`] density so both paths measure the same air.
const EXTINCTION: f32 = 0.007;

/// What saturated smog looks like: warm ember red — the lava-lit haze. Mirrors
/// `FOG_RGB` in `atmosphere_fog.wgsl` (linear 0.2633, 0.0331, 0.0116).
const FOG_COLOR: Color = Color::srgb(0.55, 0.20, 0.11);

/// The sky dome (`sky.wgsl`): smog + stars from the atmosphere integral, per direction.
/// The one uniform is the room's visual floating-origin offset (xyz), folding the
/// camera back into the true planet frame during a rebased ascent.
#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct SkyMaterial {
    #[uniform(0)]
    frame_offset: Vec4,
}

impl Material for SkyMaterial {
    fn vertex_shader() -> ShaderRef {
        SKY_SHADER_HANDLE.into()
    }

    fn fragment_shader() -> ShaderRef {
        SKY_SHADER_HANDLE.into()
    }

    // Opaque: the dome IS the sky (pinned to the far plane, so all geometry wins the
    // depth test and transparents blend over it).
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }

    // A camera-relative dome writes no useful prepass depth and casts no shadows — and
    // its custom vertex shader doesn't match the default prepass/shadow interface, so
    // opt out of both (same reasoning as the ash field).
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    // The dome is viewed from the *inside*, so its outward-wound triangles are all
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

/// Spawn the one sky-dome mesh (a unit sphere; the shader ignores its transform,
/// re-centres it on the camera, and pins it to the far plane). `NoFrustumCulling`
/// because the shader moves the vertices far from the baked AABB.
fn spawn_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(1.0).mesh().ico(4).unwrap())),
        MeshMaterial3d(materials.add(SkyMaterial { frame_offset: Vec4::ZERO })),
        NoFrustumCulling,
        Name::new("Sky dome"),
    ));
}

/// Drive the near-field pieces from the camera's local air, and the sky from the
/// floating-origin frame. One `atmosphere_fraction` evaluation feeds the
/// [`DistanceFog`] density and the ash cull so they can never disagree; asset writes
/// are gated on change (mutating an asset flags a GPU re-upload).
fn update_atmosphere(
    mut commands: Commands,
    // Present only in multiplayer (the floating-origin frame); single-player true == local.
    frame: Option<Res<crate::net::ClientRoomFrame>>,
    mut cameras: Query<
        (Entity, &GlobalTransform, Option<&mut DistanceFog>),
        (With<Camera3d>, With<PrimaryEguiContext>),
    >,
    mut ash: ResMut<Assets<AshMaterial>>,
    mut skies: ResMut<Assets<SkyMaterial>>,
) {
    let offset = frame.map(|f| f.offset.as_vec3()).unwrap_or(Vec3::ZERO);
    let Some((entity, transform, fog)) = cameras.iter_mut().next() else {
        return;
    };
    // The camera's TRUE position (frame offset folded back in), so the smog is measured
    // at real altitude even under a rebase. OPTICAL profile: the visuals track the
    // aerosols (H/2), not the drag gas.
    let fraction = atmosphere_optical_fraction(transform.translation() + offset);

    // Near-field fog for plain PBR meshes: density = extinction × the air at the
    // camera — exact for the short paths those meshes live at (the long-sightline
    // surfaces integrate the real profile in their own shaders).
    let density = EXTINCTION * fraction;
    match fog {
        Some(mut fog) => fog.falloff = FogFalloff::Exponential { density },
        None => {
            commands.entity(entity).try_insert(DistanceFog {
                color: FOG_COLOR,
                falloff: FogFalloff::Exponential { density },
                ..default()
            });
        }
    }

    // Ash thins with the air (its shader culls flakes by this fraction).
    let ash_ids: Vec<_> = ash.ids().collect();
    for id in ash_ids {
        if ash.get(id).is_some_and(|m| m.density() != fraction) {
            if let Some(mut m) = ash.get_mut(id) {
                m.set_density(fraction);
            }
        }
    }

    // The sky integrates from the camera's true position: hand it the frame offset.
    let target = offset.extend(0.0);
    let sky_ids: Vec<_> = skies.ids().collect();
    for id in sky_ids {
        if skies.get(id).is_some_and(|m| m.frame_offset != target) {
            if let Some(mut m) = skies.get_mut(id) {
                m.frame_offset = target;
            }
        }
    }
}

/// Register the fog library + sky material and the atmosphere systems.
pub fn plugin(app: &mut App) {
    // The shared transmittance library first — sky + magma `#import` it.
    bevy::asset::load_internal_asset!(
        app,
        ATMOSPHERE_FOG_SHADER_HANDLE,
        "../../assets/atmosphere_fog.wgsl",
        Shader::from_wgsl
    );
    bevy::asset::load_internal_asset!(
        app,
        SKY_SHADER_HANDLE,
        "../../assets/sky.wgsl",
        Shader::from_wgsl
    );
    app.add_plugins(MaterialPlugin::<SkyMaterial>::default())
        .add_systems(Startup, spawn_sky)
        .add_systems(Update, update_atmosphere);
}
