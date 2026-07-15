use bevy::{
    asset::uuid_handle,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

/// The procedural magma shader (`client/assets/magma_material.wgsl`), embedded at
/// compile time by `RenderMainPassPlugin`. Extends `StandardMaterial` so the planet
/// sits on the normal PBR path (lit by the same faint sun + warm ambient the rest of
/// the scene uses); the extension computes a rough black-rock `base_color` and paints
/// glowing magma into noise-valley rivulets via `emissive` (which flows over time).
pub const MAGMA_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("a3f1c9d2-7e6b-4a58-9c3d-1f2e4b6a8c05");

pub type MagmaMaterial = ExtendedMaterial<StandardMaterial, MagmaExtension>;

/// Mirrors `MagmaParams` in the WGSL field-for-field (`ShaderType` derives the
/// std140/WebGL2 layout from the field order, so a mismatch fails at pipeline
/// creation rather than rendering wrong). All colours are linear-space.
#[derive(ShaderType, Debug, Clone)]
pub struct MagmaParams {
    /// Rough black basalt between the rivulets.
    rock_color: LinearRgba,
    /// Cooling magma along the rivulet edges (dull orange-red).
    magma_color: LinearRgba,
    /// White-hot core of the brightest channels.
    hot_color: LinearRgba,
    /// 1 / basalt-mottle feature size (m⁻¹).
    rock_freq: f32,
    /// 1 / rivulet feature size (m⁻¹) — how wide the magma channels are.
    flow_freq: f32,
    /// Domain-scroll speed of the magma flow (m/s), so channels creep over time.
    flow_speed: f32,
    /// Domain-warp strength — meanders the channels so they aren't straight noise.
    warp: f32,
    /// Rivulet coverage: the ridged-noise band `[lo, hi]` that reads as molten.
    /// Higher `lo` = thinner, sparser veins.
    rivulet_lo: f32,
    rivulet_hi: f32,
    /// Magma emissive strength (kept "dull" — a low value so it glows, not blinds).
    emissive_strength: f32,
    /// Time-driven flicker depth of the glow [0, 1].
    flicker: f32,
    /// The room's visual floating-origin offset (xyz; w padding). The planet meshes
    /// ride the ground, which the client parks at `-offset`, so the triplanar noise
    /// would slide across the surface without adding this back. Driven by
    /// `sync_visual_room_frame`.
    frame_offset: Vec4,
}

impl Default for MagmaParams {
    fn default() -> Self {
        MagmaParams {
            rock_color: Color::srgb(0.015, 0.012, 0.012).to_linear(),
            magma_color: Color::srgb(0.85, 0.20, 0.03).to_linear(),
            hot_color: Color::srgb(1.0, 0.65, 0.25).to_linear(),
            // ~7 m basalt clumps, ~14 m rivulet channels.
            rock_freq: 1.0 / 7.0,
            flow_freq: 1.0 / 14.0,
            flow_speed: 0.35,
            warp: 6.0,
            rivulet_lo: 0.72,
            rivulet_hi: 0.95,
            emissive_strength: 2.2,
            flicker: 0.25,
            frame_offset: Vec4::ZERO,
        }
    }
}

#[derive(Asset, AsBindGroup, Debug, Clone, Default, TypePath)]
pub struct MagmaExtension {
    #[uniform(100)]
    params: MagmaParams,
}

impl crate::render_main_pass::FrameOffsetMaterial for MagmaMaterial {
    /// Full xyz (the triplanar sample uses all three planes). Kept raw (no modulo) —
    /// the triplanar FBM isn't periodic, and once the offset is large enough to lose
    /// f32 precision the planet is a distant dot whose fine rivulets aren't resolvable.
    fn frame_target(&self, visual_offset: bevy::math::DVec3) -> Vec4 {
        visual_offset.as_vec3().extend(0.0)
    }

    fn stored_frame(&self) -> Vec4 {
        self.extension.params.frame_offset
    }

    fn set_stored_frame(&mut self, value: Vec4) {
        self.extension.params.frame_offset = value;
    }
}

impl MaterialExtension for MagmaExtension {
    fn fragment_shader() -> ShaderRef {
        MAGMA_SHADER_HANDLE.into()
    }
}

/// A ready-to-use magma material instance for the planet + cliffs: the black-rock
/// PBR base (fully rough, no metal, no specular sheen) plus the flowing-magma
/// extension. One instance, so no per-instance seeding is needed.
pub fn magma_material() -> MagmaMaterial {
    ExtendedMaterial {
        base: StandardMaterial {
            perceptual_roughness: 1.0,
            metallic: 0.0,
            reflectance: 0.02,
            // The magma shader applies the EXACT atmosphere integral itself (the planet
            // is the one long-sightline surface); Bevy's camera-uniform DistanceFog
            // would double-fog it.
            fog_enabled: false,
            ..Default::default()
        },
        extension: MagmaExtension::default(),
    }
}
