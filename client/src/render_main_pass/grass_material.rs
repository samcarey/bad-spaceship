use bevy::{
    asset::uuid_handle,
    color::Mix,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

/// The procedural turf shader (`client/assets/grass_material.wgsl`), embedded
/// at compile time by `RenderMainPassPlugin` — see the WGSL header for
/// provenance (CC0 turf parallax + MIT noise). Extending `StandardMaterial`
/// keeps the ground on the normal PBR path (sun, ambient, received shadows),
/// exactly like the old textured material; the extension only computes
/// `base_color` procedurally.
pub const GRASS_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("18ed3426-9431-409f-80e1-58e22b60d713");

pub type GrassMaterial = ExtendedMaterial<StandardMaterial, GrassExtension>;

/// Mirrors `GrassParams` in the WGSL field-for-field (`ShaderType` computes
/// the std140/WebGL2 layout from the names, so a mismatch fails loudly at
/// pipeline creation instead of rendering wrong). Values the shader would
/// otherwise derive per fragment — frequencies, the faded mean colour — are
/// precomputed here once.
#[derive(ShaderType, Debug, Clone)]
pub struct GrassParams {
    /// Deepest turf colour (what shows between/below blades), linear-space.
    base_color: LinearRgba,
    /// Blade-tip colour at the surface.
    highlight_color: LinearRgba,
    /// Patchy variation tint (drier/yellower grass).
    dry_color: LinearRgba,
    /// What the turf averages out to; distant fragments fade to this.
    mean_color: LinearRgba,
    /// 1 / clump feature size (m⁻¹).
    clump_freq: f32,
    /// 1 / blade feature size (m⁻¹).
    blade_freq: f32,
    /// Parallax turf depth (m); the shader splits it across its layers.
    turf_depth: f32,
    /// Surface coverage threshold: higher = sparser tips, deeper shadows.
    threshold: f32,
    /// Blade-vs-clump noise mix [0, 1].
    blade_mix: f32,
    /// 1 / dry-patch feature size (m⁻¹).
    dry_patch_freq: f32,
    /// Dry-patch tint strength [0, 1].
    dry_strength: f32,
    /// Distance (m) where the fade to `mean_color` starts / completes. Kills
    /// far-field shimmer (procedural noise has no mipmaps) and lets far
    /// fragments skip the parallax march.
    fade_start: f32,
    fade_end: f32,
}

impl Default for GrassParams {
    fn default() -> Self {
        // Tuned by screenshot A/B against the old grass.png photo texture.
        let base_color = Color::srgb(0.045, 0.15, 0.035).to_linear();
        let highlight_color = Color::srgb(0.33, 0.55, 0.18).to_linear();
        GrassParams {
            mean_color: base_color.mix(&highlight_color, 0.5),
            base_color,
            highlight_color,
            dry_color: Color::srgb(0.40, 0.48, 0.17).to_linear(),
            clump_freq: 1.0 / 0.5,
            blade_freq: 1.0 / 0.035,
            turf_depth: 0.06,
            threshold: 0.68,
            blade_mix: 0.70,
            dry_patch_freq: 1.0 / 7.0,
            dry_strength: 0.35,
            fade_start: 25.0,
            fade_end: 55.0,
        }
    }
}

#[derive(Asset, AsBindGroup, Debug, Clone, Default, TypePath)]
pub struct GrassExtension {
    #[uniform(100)]
    params: GrassParams,
}

impl MaterialExtension for GrassExtension {
    fn fragment_shader() -> ShaderRef {
        GRASS_SHADER_HANDLE.into()
    }
}
