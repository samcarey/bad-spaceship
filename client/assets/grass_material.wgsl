// Procedural grass ground shader — replaces the 1.15 MB grass.png photo
// texture with ~zero bytes of math, and upgrades the flat image to a turf
// with parallax depth (blades occlude each other as the view tilts).
//
// Adapted from two open-source shaders:
//  - "Flat Parallax Turf" (godotshaders.com/shader/flat-parallax-turf/, CC0):
//    the layered-parallax turf idea — march a view-tilted offset through
//    `layers` slices, and the first slice whose noise pokes above a
//    depth-shrinking threshold is the blade tip you see; deeper hits shade
//    darker. Its three input noise *textures* are replaced here with inline
//    procedural noise, and its tangent-space view offset is rebuilt in world
//    space (the ground's grass-space UV is just world XZ, so no mesh
//    tangents are needed).
//  - the shared noise library (`noise.wgsl`, ported from
//    tuxalin/procedural-tileable-shaders, MIT) replaces the noise those
//    textures would have contained.
//
// This is a `MaterialExtension` on StandardMaterial: only `base_color` is
// computed here, then the standard PBR path (sun + ambient + received
// shadows + tonemapping) runs unchanged — matching how the old textured
// material was lit.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    mesh_view_bindings::view,
}
#import bad_spaceship::noise::{vnoise, fbm}

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

// Mirrors `GrassParams` in `grass_material.rs` field-for-field; see the Rust
// doc comments for what each knob does. Frequencies and the mean colour are
// precomputed CPU-side — nothing here is derived per fragment.
struct GrassParams {
    base_color: vec4<f32>,
    highlight_color: vec4<f32>,
    dry_color: vec4<f32>,
    mean_color: vec4<f32>,
    clump_freq: f32,
    blade_freq: f32,
    turf_depth: f32,
    threshold: f32,
    blade_mix: f32,
    dry_patch_freq: f32,
    dry_strength: f32,
    fade_start: f32,
    fade_end: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> grass: GrassParams;

// Parallax slice count. A compile-time constant (not a uniform) so the march
// below is a statically-bounded, unrollable loop for mobile GLSL compilers.
const LAYERS: i32 = 8;
const INV_LAYERS: f32 = 1.0 / f32(LAYERS);

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let world = in.world_position.xyz;
    let rel = view.world_position - world;
    let dist = length(rel);

    // Procedural noise has no mipmaps, so distant fragments shimmer as whole
    // blades flicker per pixel; fade to the mean turf colour with distance.
    // Fully-faded fragments skip the parallax march entirely (mobile is the
    // floor this has to run on).
    let dist_fade = smoothstep(grass.fade_start, grass.fade_end, dist);

    var albedo = grass.mean_color.rgb;
    if dist_fade < 1.0 {
        // The turf shader's tangent-space parallax, in world space: a view ray
        // that continues `d` metres below the surface exits at
        // xz - v.xz * (d / v.y). Clamp v.y so grazing angles don't smear the
        // offset to infinity (the Godot original divides by the raw normal dot).
        let v = rel / dist;
        let uv_step = (v.xz / max(v.y, 0.25)) * (grass.turf_depth * INV_LAYERS);

        var uv = world.xz;
        var turf = grass.base_color.rgb;
        var depth_ratio = 0.0;
        for (var i = 0; i < LAYERS; i++) {
            let clumps = fbm(uv * grass.clump_freq);
            let blades = vnoise(uv * grass.blade_freq);
            let combined = mix(clumps, blades, grass.blade_mix);
            // Deeper slices need less noise to poke through (the blades narrow
            // toward the tip), so the threshold shrinks with depth.
            if combined > grass.threshold * (1.0 - depth_ratio) {
                // `patch` is a reserved WGSL keyword, hence `patchiness`.
                let patchiness = vnoise(world.xz * grass.dry_patch_freq);
                let tint = mix(grass.highlight_color.rgb, grass.dry_color.rgb, patchiness * grass.dry_strength);
                turf = mix(tint, grass.base_color.rgb, depth_ratio);
                break;
            }
            depth_ratio += INV_LAYERS;
            uv -= uv_step;
        }
        albedo = mix(turf, grass.mean_color.rgb, dist_fade);
    }
    pbr_input.material.base_color = vec4(albedo, 1.0);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
