// Procedural metal shader for the loose building parts. Each part's look —
// tint, brushing, sparkle flakes, scratches — derives deterministically from a
// per-part seed minted at spawn (`PartSeed` / `NetPart::seed`), so parts vary
// like the old per-part random `StandardMaterial`s but with machined surface
// texture, and every multiplayer client renders the same part identically.
//
// Like the grass, this is a `MaterialExtension` on StandardMaterial: it only
// perturbs `base_color` and `perceptual_roughness` before the stock PBR path
// (sun, ambient, shadows, tonemapping) runs. The per-part *scalar* look
// (tint, metallic, base roughness) stays on the base StandardMaterial — which
// is also what lets the focus-highlight systems keep recoloring parts by
// writing `base.base_color`/`emissive` exactly as before.
//
// Cost: three value-noise evaluations per fragment, no loops, no textures —
// cheaper than the grass turf march (mobile WebGL2 is the floor).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
}
#import bad_spaceship::noise::vnoise

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

// Mirrors `MetalParams` in `metal_material.rs` field-for-field; see the Rust
// doc comments. All values are pre-randomized per part on the CPU.
struct MetalParams {
    brush_cos: f32,
    brush_sin: f32,
    brush_freq: f32,
    brush_strength: f32,
    flake_freq: f32,
    flake_strength: f32,
    scratch_strength: f32,
    noise_offset: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> metal: MetalParams;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Per-face UVs (Bevy's cuboid mesh), rotated by the part's brush angle so
    // the machining direction varies part to part.
    let uv = vec2(
        in.uv.x * metal.brush_cos - in.uv.y * metal.brush_sin,
        in.uv.x * metal.brush_sin + in.uv.y * metal.brush_cos,
    ) + metal.noise_offset;

    // Brushed-metal streaks: value noise squeezed hard along one axis reads as
    // fine parallel grinding lines. They modulate brightness and roughness in
    // opposite senses (a groove is darker and rougher than a ridge).
    let streak = vnoise(vec2(uv.x * metal.brush_freq, uv.y * 3.0)) - 0.5;
    var color = pbr_input.material.base_color.rgb * (1.0 + streak * metal.brush_strength);
    var roughness = pbr_input.material.perceptual_roughness + streak * metal.brush_strength * 0.5;

    // Galvanized/flake sparkle: rare bright cells of high-frequency noise get a
    // brightness kick and a polished (low-roughness) spot.
    let flake = step(0.82, vnoise(uv * metal.flake_freq)) * metal.flake_strength;
    color += vec3(flake * 0.35);
    roughness -= flake * 0.5;

    // Scratches: sparse thin lines across the brushing direction — bright,
    // rough gouges down to bare metal.
    let scratch = smoothstep(0.87, 0.93, vnoise(vec2(uv.x * 2.3, uv.y * metal.brush_freq * 0.6) + 13.7))
        * metal.scratch_strength;
    color = mix(color, vec3(0.75), scratch);
    roughness += scratch * 0.4;

    pbr_input.material.base_color = vec4(color, pbr_input.material.base_color.a);
    pbr_input.material.perceptual_roughness = clamp(roughness, 0.04, 1.0);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
