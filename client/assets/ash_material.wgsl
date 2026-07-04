// Falling volcanic ash — a camera-following field of little grey flakes that
// flutter down, to give a sense of 3D space and self-motion when flying through
// an otherwise featureless sky (a volcano erupted nearby).
//
// Entirely GPU-driven: the CPU bakes ONE static mesh of `NUM_FLAKES` billboard
// quads (see `ash_material.rs`) and never touches it again. Each flake's motion
// is recomputed here every frame from its per-vertex seed + `globals.time`, so
// the whole effect is a couple of uniforms and one draw call — no per-frame CPU
// work, no simulation, nothing networked.
//
// The flakes live in an invisible box (`box_size`) that is re-centred on the
// camera each frame by wrapping every flake's lattice position into
// [-box/2, box/2] around the camera (`v - box*round(v/box)`). So the field is
// effectively infinite but only ~NUM_FLAKES flakes ever exist, always the ones
// nearest you, out to roughly the map size.
//
// Depth: rendered as a transparent material, so it is depth-TESTED against the
// opaque scene for free — a flake behind the ground / a part / a monster is
// correctly hidden — while not writing depth itself (flakes don't occlude each
// other, which is imperceptible for faint grey specks and costs nothing extra).

#import bevy_pbr::mesh_view_bindings::{view, globals}

struct AshParams {
    // Ash colour (linear); `.a` is the master opacity.
    color: vec4<f32>,
    // Field box dimensions in metres (xyz); w unused.
    box_size: vec4<f32>,
    // Flake radius min/max (metres).
    size_min_max: vec2<f32>,
    // Fall speed min/max (metres/second).
    fall_min_max: vec2<f32>,
    // Horizontal flutter amplitude (metres) and angular frequency.
    sway_amp: f32,
    sway_freq: f32,
    // Flicker (fake spin) angular frequency — flakes brighten/shrink as they
    // "turn edge-on", which reads as tumbling and sells the descent.
    spin_freq: f32,
    // Flakes closer than this (metres) fade out, so none smear across your face.
    near_fade: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> ash: AshParams;

struct Vertex {
    // .x = per-flake seed (identical across the quad's 4 corners); .yz unused.
    @location(0) position: vec3<f32>,
    // Corner in [0,1]²; billboards to [-1,1]² and doubles as the disc UV.
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
    @location(2) alpha: f32,
};

// Dave Hoskins' hash (hash31, https://www.shadertoy.com/view/4djSRW, CC0):
// one float seed -> three decorrelated randoms in [0,1).
fn hash31(p: f32) -> vec3<f32> {
    var p3 = fract(vec3<f32>(p) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.xxy + p3.yzz) * p3.zyx);
}

const TAU: f32 = 6.2831853;

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    let seed = v.position.x;
    let r = hash31(seed);
    let r2 = hash31(seed + 11.7);
    let box = ash.box_size.xyz;
    let t = globals.time;
    let cam = view.world_position;

    let fall = mix(ash.fall_min_max.x, ash.fall_min_max.y, r.x);
    let size = mix(ash.size_min_max.x, ash.size_min_max.y, r.y);
    let phase = r2.x * TAU;

    // Fixed lattice offset for this flake, drifting down and fluttering. Two
    // out-of-phase sines give a wobbly, leaf-like descent rather than a plumb drop.
    let base = r * box;
    let drift = vec3<f32>(
        ash.sway_amp * sin(t * ash.sway_freq + phase),
        -t * fall,
        ash.sway_amp * cos(t * ash.sway_freq * 0.8 + phase * 1.7),
    );

    // Re-centre the tiled lattice on the camera: pick the copy nearest us.
    let rel = base + drift - cam;
    let wrapped = rel - box * round(rel / box);
    let center = cam + wrapped;

    // Real tumble: an in-plane spin plus an edge-on foreshorten, so each flake
    // visibly turns over as it falls (not just a brightness flicker). `tumble`
    // shrinks the flake's height toward a thin sliver when it's edge-on; a small
    // floor keeps it from fully vanishing.
    let roll = t * ash.spin_freq * 0.2 + phase;
    let tumble = 0.12 + 0.88 * abs(cos(t * ash.spin_freq * 0.3 + r2.y * TAU));

    // Billboard the corner in the camera's right/up plane (columns of the
    // camera-to-world matrix), so every flake faces the viewer. Foreshorten the
    // corner along the flake's own axis, then spin it in the billboard plane —
    // the fragment draws its shape in the untransformed UV, so it inherits this
    // spin + squish and reads as a tumbling chip.
    let right = view.world_from_view[0].xyz;
    let up = view.world_from_view[1].xyz;
    var corner = v.uv * 2.0 - 1.0;
    corner.y = corner.y * tumble;
    let cr = cos(roll);
    let sr = sin(roll);
    corner = vec2<f32>(corner.x * cr - corner.y * sr, corner.x * sr + corner.y * cr);
    let world = center + (corner.x * right + corner.y * up) * size;

    // Fades: pull flakes out of your face up close, and dissolve them toward the
    // box walls so nothing pops in/out at the wrap seams.
    let dist = length(wrapped);
    let near = smoothstep(0.0, ash.near_fade, dist);
    let n = abs(wrapped) / (box * 0.5);
    let edge = 1.0 - smoothstep(0.7, 1.0, max(max(n.x, n.y), n.z));

    var out: VertexOutput;
    out.clip_position = view.clip_from_world * vec4<f32>(world, 1.0);
    out.uv = v.uv;
    // Two-tone: half the flakes a darker grey, half a lighter grey (a per-flake
    // coin flip, scaled off the base colour so the AshParams knob still tunes it).
    out.tint = ash.color.rgb * select(1.35, 0.6, r2.z < 0.5);
    out.alpha = ash.color.a * near * edge * (0.4 + 0.6 * tumble);
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Crisp-edged flake (a small chip) from the corner UV. The quad is spun and
    // foreshortened in the vertex shader, so this axis-aligned diamond reads as a
    // tumbling flake on screen (vs the old soft round dot).
    let p = (in.uv - vec2<f32>(0.5)) * 2.0;
    let d = abs(p.x) + abs(p.y);
    let disc = 1.0 - smoothstep(0.82, 1.0, d);
    let a = in.alpha * disc;
    if a < 0.003 {
        discard;
    }
    return vec4<f32>(in.tint, a);
}
