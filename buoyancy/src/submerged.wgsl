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

// rgb = water colour (linear), w = waterline height (world y). Fed from
// WATER_COLOR / WATER_LEVEL in main.rs.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: vec4<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    if in.world_position.y < water.w {
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
