//! Falling volcanic-ash flakes (`client/assets/ash_material.wgsl`).
//!
//! A single static mesh of `NUM_FLAKES` billboard quads, animated entirely in
//! the vertex shader from a per-vertex seed + time. The mesh sits at the origin
//! but every flake is positioned in world space relative to the camera by the
//! shader, so the field always surrounds the viewer out to ~the map size — see
//! the WGSL header for the full story. Rendered transparent so it depth-tests
//! against the opaque scene (background flakes are hidden by nearer geometry)
//! for free, and unlit so the ash stays an even grey regardless of the sun.

use bevy::{
    asset::{uuid_handle, RenderAssetUsages},
    camera::visibility::NoFrustumCulling,
    mesh::{Indices, MeshVertexBufferLayoutRef},
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

use bad_spaceship_shared::map::PLATFORM_WIDTH_M;

pub const ASH_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("7b3f9c22-4d81-4a6e-9f0c-2a1e6b5d8c40");

/// How many flakes exist at once. They tile around the camera, so this is a
/// density knob, not a world budget — the field is effectively infinite. ~4k
/// tiny transparent quads is cheap even on the WebGL2 mobile floor.
const NUM_FLAKES: usize = 4000;

/// Mirrors `AshParams` in the WGSL field-for-field (`ShaderType` derives the
/// std140/WebGL2 layout from the names, so a mismatch fails loudly at pipeline
/// creation rather than rendering wrong).
#[derive(ShaderType, Debug, Clone)]
pub struct AshParams {
    /// Ash grey (linear); `.a` is the master opacity.
    color: LinearRgba,
    /// Field box size in metres (xyz); w unused/padding.
    box_size: Vec4,
    /// Flake radius min/max (m).
    size_min_max: Vec2,
    /// Fall speed min/max (m/s).
    fall_min_max: Vec2,
    /// Horizontal flutter amplitude (m) and angular frequency.
    sway_amp: f32,
    sway_freq: f32,
    /// Flicker (fake-spin) angular frequency.
    spin_freq: f32,
    /// Flakes closer than this (m) fade out.
    near_fade: f32,
}

impl Default for AshParams {
    fn default() -> Self {
        AshParams {
            // White ash, mostly opaque. Note the per-flake grey jitter in the
            // shader (`tint * mix(0.75, 1.2, r)`) keeps some flakes off pure
            // white so the field still has depth.
            color: Color::WHITE.with_alpha(0.85).to_linear(),
            // A box ~2x the platform width, so flakes surround you out to ~one
            // map-radius (half the box) in every direction.
            box_size: Vec4::new(PLATFORM_WIDTH_M * 2.0, PLATFORM_WIDTH_M * 1.8, PLATFORM_WIDTH_M * 2.0, 0.0),
            size_min_max: Vec2::new(0.02, 0.07), // 2–7 cm flakes
            fall_min_max: Vec2::new(0.5, 1.3),   // slow ashy flutter
            sway_amp: 0.35,
            sway_freq: 1.4,
            spin_freq: 7.0,
            near_fade: 1.5,
        }
    }
}

#[derive(Asset, AsBindGroup, Debug, Clone, TypePath)]
pub struct AshMaterial {
    #[uniform(0)]
    params: AshParams,
}

impl Material for AshMaterial {
    fn vertex_shader() -> ShaderRef {
        ASH_SHADER_HANDLE.into()
    }

    fn fragment_shader() -> ShaderRef {
        ASH_SHADER_HANDLE.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    // Keep the flakes out of the depth-prepass and shadow passes. Both use the
    // engine's *default* prepass/shadow vertex shader, not ours — so they ignore
    // the per-flake vertex animation (the flakes only live at their real world
    // positions because THIS material's vertex shader scatters them around the
    // camera) and, worse, our `specialize` below remaps UV to shader-location 2,
    // which doesn't match the default prepass shader's vertex↔fragment interface.
    // The result was an invalid `prepass_pipeline` that quit the whole app on GPUs
    // that build one (validated on WebKit; would fail on the iPhone too). A
    // transparent, unlit, camera-relative particle field has no business writing
    // depth or casting shadows anyway, so simply opt out of both passes.
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    // The mesh carries only a packed seed (in POSITION.x) and the corner UV;
    // pin those to the shader locations the WGSL reads (0 and 2).
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}

/// Build the static flake mesh: one quad per flake, all four corners sharing the
/// flake's seed (packed into POSITION.x) and carrying their [0,1]² corner UV.
fn build_flake_mesh() -> Mesh {
    let mut positions = Vec::with_capacity(NUM_FLAKES * 4);
    let mut uvs = Vec::with_capacity(NUM_FLAKES * 4);
    let mut indices = Vec::with_capacity(NUM_FLAKES * 6);

    for i in 0..NUM_FLAKES {
        // Distinct, non-tiny seed per flake; the shader hashes it, so the exact
        // value only needs to be well-spread.
        let seed = i as f32 * 0.7371 + 0.5;
        for corner in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
            positions.push([seed, 0.0, 0.0]);
            uvs.push(corner);
        }
        let base = (i * 4) as u32;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    let mut mesh = Mesh::new(
        bevy::render::render_resource::PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

pub fn spawn_ash_field(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<AshMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(build_flake_mesh())),
        MeshMaterial3d(materials.add(AshMaterial {
            params: AshParams::default(),
        })),
        // The baked mesh's AABB hugs the origin, but the shader scatters flakes
        // around the *camera* — so once the player flies away from the origin the
        // whole mesh would be frustum-culled and the ash would vanish. Disable
        // culling: the effect is one cheap draw call that should always run.
        NoFrustumCulling,
        Name::new("Ash field"),
    ));
}
