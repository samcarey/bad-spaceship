// Fragment extension for the shell materials: tint everything below the
// waterline toward the water's colour and thicken its alpha, as the water
// column in front of the hull would. This runs per-pixel in the shell's own
// shader because alpha-blended meshes are sorted whole-mesh, so the water
// volume can never composite over just the submerged part of an intersecting
// hull (see `SubmergedTint` in main.rs, which supplies the uniform).
//
// Forward pass only: the extension overrides `MaterialExtension::
// fragment_shader` alone, and a Blend material never renders in the opaque
// prepass, so no prepass/deferred variants are needed.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

// rgb = water colour (linear), w = still-water level (world y). Fed from
// WATER_COLOR / WATER_LEVEL in main.rs.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: vec4<f32>;
// Wave field, so the tint boundary follows the moving surface: x = amplitude,
// y = wavenumber k, z = accumulated phase Φ, w = spatial origin x₀ (world x
// under the frustum's centre). Kept current by `animate_waves` in main.rs;
// all-zero while the water is flat.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> wave: vec4<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Local waterline: still level + A*sin(k*(x - x0) - phase), matching
    // WaveField::height on the CPU side (water.w = still level, wave.w = x0).
    let level = water.w + wave.x * sin(wave.y * (in.world_position.x - wave.w) - wave.z);
    if in.world_position.y < level {
        pbr_input.material.base_color = vec4<f32>(
            mix(pbr_input.material.base_color.rgb, water.rgb, 0.5),
            min(pbr_input.material.base_color.a + 0.25, 1.0),
        );
    }

    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
