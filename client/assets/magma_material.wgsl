// Procedural magma-planet shader — rough black basalt with dull molten rivulets
// that slowly flow. A `MaterialExtension` on StandardMaterial, so only `base_color`
// and `emissive` are computed here and the standard PBR path (faint sun + warm
// ambient + shadows + tonemapping) runs unchanged, matching the rest of the scene.
//
// The planet is a giant low-poly sphere and the cliffs are vertical walls, so the
// surface is sampled **triplanar** (blend three world-plane projections by the
// world normal): the magma flows *along* the flat planet top and *down* the cliff
// faces from one shared material. `globals.time` scrolls the flow — no per-frame CPU.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    mesh_view_bindings::{view, globals},
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

// Mirrors `MagmaParams` in `magma_material.rs` field-for-field.
struct MagmaParams {
    rock_color: vec4<f32>,
    magma_color: vec4<f32>,
    hot_color: vec4<f32>,
    rock_freq: f32,
    flow_freq: f32,
    flow_speed: f32,
    warp: f32,
    rivulet_lo: f32,
    rivulet_hi: f32,
    emissive_strength: f32,
    flicker: f32,
    // The room's visual floating-origin offset (xyz; w padding). The planet meshes
    // are children of the ground, which is parked at `-offset` in render space every
    // frame as the room co-moves/rebases — so `world_position` slides under the
    // triplanar noise during ascent, and the molten pattern crawls across the fixed
    // planet geometry (visible against the pinned grass area). Adding it back keys
    // the noise to the TRUE planet-fixed coordinate. Full xyz: the triplanar sample
    // uses all three planes (flat top + vertical cliffs).
    frame_offset: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> magma: MagmaParams;

// Triplanar value-FBM: blend the three world-plane projections by the (squared)
// normal weights so flat tops sample XZ and vertical cliffs sample the vertical
// planes, with no seam between.
fn tri_fbm(p: vec3<f32>, w: vec3<f32>) -> f32 {
    return fbm(p.zy) * w.x + fbm(p.xz) * w.y + fbm(p.xy) * w.z;
}

// One plane's molten-rivulet value: meander the domain with a low-frequency warp
// (so channels snake instead of reading as raw noise), scroll it over time, then
// fold the FBM into *ridged* noise — a value that peaks in thin ridges, which read
// as the glowing veins between basalt.
fn ridged_plane(uv0: vec2<f32>, warp: f32, scroll: vec2<f32>) -> f32 {
    let wv = vec2(
        fbm(uv0 * 0.35 + vec2(11.0, 7.0)),
        fbm(uv0 * 0.35 + vec2(23.0, 41.0)),
    ) - 0.5;
    let uv = uv0 + wv * warp + scroll;
    let v = fbm(uv);
    return 1.0 - abs(2.0 * v - 1.0);
}

fn tri_ridged(p: vec3<f32>, w: vec3<f32>, warp: f32, scroll: vec2<f32>) -> f32 {
    return ridged_plane(p.zy, warp, scroll) * w.x
        + ridged_plane(p.xz, warp, scroll) * w.y
        + ridged_plane(p.xy, warp, scroll) * w.z;
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Sample in the planet-fixed frame so the magma doesn't slide as the floating
    // origin co-moves the ground under it (see `frame_offset`). The `globals.time`
    // scroll below is the intended flow and stays independent of this.
    let p = in.world_position.xyz + magma.frame_offset.xyz;
    let n = normalize(in.world_normal);
    let t = globals.time;

    // Triplanar weights, sharpened so the blend band is narrow.
    let wn = pow(abs(n), vec3(4.0));
    let w = wn / (wn.x + wn.y + wn.z);

    // Rough black basalt: dark, faintly mottled (static).
    let rock_n = tri_fbm(p * magma.rock_freq, w);
    let rock = magma.rock_color.rgb * (0.6 + 0.9 * rock_n);

    // Molten rivulets: domain-warped ridged noise, scrolled by time. On the flat
    // top the scroll runs the channels along the ground; on the cliffs it runs
    // them downward (the plane's second coord is world Y there) — magma dripping.
    let scroll = vec2(0.0, t * magma.flow_speed) * magma.flow_freq;
    let ridged = tri_ridged(p * magma.flow_freq, w, magma.warp * magma.flow_freq, scroll);
    let mask = smoothstep(magma.rivulet_lo, magma.rivulet_hi, ridged);
    let core = smoothstep(magma.rivulet_hi, 1.0, ridged);

    // Gentle flicker so the glow breathes rather than sitting flat.
    let flick = 1.0 - magma.flicker * 0.5 * (1.0 + sin(t * 3.0 + ridged * 20.0));

    let glow = mix(magma.magma_color.rgb, magma.hot_color.rgb, core);
    let emissive = glow * mask * magma.emissive_strength * flick;

    // Char the rock toward the magma tint inside the channels so the albedo reads
    // hot even before lighting; the emissive does the actual glowing.
    pbr_input.material.base_color = vec4(mix(rock, magma.magma_color.rgb * 0.15, mask), 1.0);
    pbr_input.material.emissive = vec4(emissive, 0.0);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
