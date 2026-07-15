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
#import bad_spaceship::atmosphere::{transmittance, fog_radiance}

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

// Distance level-of-detail: the triplanar FBM/ridged noise has no mipmaps, so from
// afar the fine basalt mottling and molten veins shimmer and alias. The octave budget
// steps down across this range (see `magma_fbm`) — several distinct detail levels — and
// past LOD_END the noise is gone entirely, the receding planet a smooth uniform red glow
// (which also makes the lava light "shine through" the haze from orbit rather than
// resolving as sharp channels). The bands are spaced so each octave holds for a while
// before the next drops, so the levels read as levels.
const LOD_START: f32 = 60.0;
const LOD_END: f32 = 1800.0;
// Finer octaves above the base that fade out with distance — the number of LOD levels.
const LOD_OCTAVES: f32 = 3.0;
// The rivulet mask averaged over space — the uniform glow the far surface settles to.
const MEAN_MASK: f32 = 0.35;

// Level-of-detail FBM: a base octave plus up to `LOD_OCTAVES` finer octaves, each
// faded in by the `oct` budget (octaves still active at this distance). `oct` counts
// DOWN with distance, so the surface steps through successive detail levels — full fine
// grain up close, one octave shed at a time as it recedes, a single smooth octave far
// out. Higher octaves whose weight hits zero are skipped (the loop breaks), so distant
// fragments are cheap. Renormalised by the active amplitude so the mean is stable across
// levels (no brightness pop as an octave drops).
fn magma_fbm(pos: vec2<f32>, oct: f32) -> f32 {
    var sum = vnoise(pos) * 0.6667;
    var norm = 0.6667;
    var amp = 0.6667;
    var freq = 2.0;
    var off = vec2<f32>(37.2, 17.7);
    for (var i = 0; i < 3; i++) {
        let w = clamp(oct - f32(i), 0.0, 1.0);
        if w <= 0.0 {
            break;
        }
        amp *= 0.5;
        sum += vnoise(pos * freq + off) * amp * w;
        norm += amp * w;
        freq *= 2.0;
        off = off * 1.7 + vec2<f32>(11.3, 5.1);
    }
    return sum / norm;
}

// Triplanar value-FBM (LOD-aware): blend the three world-plane projections by the
// (squared) normal weights so flat tops sample XZ and vertical cliffs sample the
// vertical planes, with no seam between.
fn tri_fbm(p: vec3<f32>, w: vec3<f32>, oct: f32) -> f32 {
    return magma_fbm(p.zy, oct) * w.x + magma_fbm(p.xz, oct) * w.y + magma_fbm(p.xy, oct) * w.z;
}

// One plane's molten-rivulet value: meander the domain with a low-frequency warp
// (so channels snake instead of reading as raw noise), scroll it over time, then
// fold the FBM into *ridged* noise — a value that peaks in thin ridges, which read
// as the glowing veins between basalt. The warp stays a coarse 2-octave `fbm` (it's
// large-scale structure); only the detail fold rolls off with `oct`, so the veins get
// broader and simpler through the LOD levels rather than just fading.
fn ridged_plane(uv0: vec2<f32>, warp: f32, scroll: vec2<f32>, oct: f32) -> f32 {
    let wv = vec2(
        fbm(uv0 * 0.35 + vec2(11.0, 7.0)),
        fbm(uv0 * 0.35 + vec2(23.0, 41.0)),
    ) - 0.5;
    let uv = uv0 + wv * warp + scroll;
    let v = magma_fbm(uv, oct);
    return 1.0 - abs(2.0 * v - 1.0);
}

fn tri_ridged(p: vec3<f32>, w: vec3<f32>, warp: f32, scroll: vec2<f32>, oct: f32) -> f32 {
    return ridged_plane(p.zy, warp, scroll, oct) * w.x
        + ridged_plane(p.xz, warp, scroll, oct) * w.y
        + ridged_plane(p.xy, warp, scroll, oct) * w.z;
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
    let world = in.world_position.xyz;
    let p = world + magma.frame_offset.xyz;
    let n = normalize(in.world_normal);
    let t = globals.time;

    // Distance LOD: 0 = full detail up close, 1 = fully smoothed far away. `oct` is the
    // octave budget — full near, shedding a level at a time, none far.
    let dist = length(view.world_position - world);
    let lod = smoothstep(LOD_START, LOD_END, dist);
    let oct = LOD_OCTAVES * (1.0 - lod);

    // The detailed basalt + rivulet noise, computed only while it's still visible
    // (far fragments skip the triplanar FBM entirely — cheap over the whole planet).
    var rock_n = 0.5; // flat-mean rock
    var ridged = 0.0;
    var mask = MEAN_MASK;
    var core = 0.0;
    if lod < 1.0 {
        // Triplanar weights, sharpened so the blend band is narrow.
        let wn = pow(abs(n), vec3(4.0));
        let w = wn / (wn.x + wn.y + wn.z);
        // Rough black basalt mottling — coarsens through the LOD levels, then to its mean.
        rock_n = mix(tri_fbm(p * magma.rock_freq, w, oct), 0.5, lod);
        // Molten rivulets: domain-warped ridged noise, scrolled by time. On the flat
        // top the scroll runs the channels along the ground; on the cliffs it runs
        // them downward (the plane's second coord is world Y there) — magma dripping.
        // The veins broaden through the LOD levels and finally dissolve to a uniform glow.
        let scroll = vec2(0.0, t * magma.flow_speed) * magma.flow_freq;
        ridged = tri_ridged(p * magma.flow_freq, w, magma.warp * magma.flow_freq, scroll, oct);
        mask = mix(smoothstep(magma.rivulet_lo, magma.rivulet_hi, ridged), MEAN_MASK, lod);
        core = mix(smoothstep(magma.rivulet_hi, 1.0, ridged), 0.0, lod);
    }
    let rock = magma.rock_color.rgb * (0.6 + 0.9 * rock_n);

    // Gentle flicker so the glow breathes rather than sitting flat — faded out with
    // distance so the far planet is a steady glow, not a shimmering one.
    let flick = 1.0 - magma.flicker * 0.5 * (1.0 + sin(t * 3.0 + ridged * 20.0)) * (1.0 - lod);

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
    // Physically-based atmosphere (bad_spaceship::atmosphere): attenuate the lit
    // surface by the EXACT transmittance integrated camera→fragment. The planet is the
    // scene's only long-sightline geometry, so it fogs by the real integral (the base
    // StandardMaterial opts out of Bevy's camera-uniform fog — magma_material()) while
    // near-field materials use DistanceFog, the same integral's short-path limit.
    // Direction/distance are frame-invariant, so render-space values serve; only the
    // camera position needs the true-frame fold.
    let t_atm = transmittance(
        view.world_position + magma.frame_offset.xyz,
        (world - view.world_position) / max(dist, 1e-3),
        dist,
    );
    out.color = vec4(fog_radiance(out.color.rgb, t_atm), out.color.a);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
