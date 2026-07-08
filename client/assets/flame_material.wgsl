// Procedural rocket-exhaust flame. The mesh is a plain unit cylinder (radius 1,
// height 1, many height segments) parented at the flare exit with -Y = exhaust;
// ALL shaping happens in the vertex stage:
//
//  - a bulge-then-taper radius profile + value-noise wobble makes the plume,
//  - length scales with the eased throttle (`strength`) and flickers with time,
//  - **ground splash**: `ground_dist` is the CPU-raycast distance from the nozzle
//    to the ground along the exhaust axis (in flame-local units; huge = no hit).
//    Vertices whose along-flame distance `s` exceeds it stop travelling down the
//    axis and flow along the ground plane instead — perpendicular to the ground
//    normal — fanning radially outward from the impact point (each vertex keeps
//    its own angular direction around the axis, projected onto the plane), so the
//    flame splashes into a spreading skirt instead of clipping through terrain.
//
// The fragment stage is an unlit white→orange→red ramp scrolled by two octaves
// of the shared value noise; the material blends ADDITIVELY (AlphaMode::Add), so
// alpha is the emission strength, overlap glows, and no depth-write is needed.

#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_clip}
#import bevy_pbr::mesh_view_bindings::globals
#import bad_spaceship::noise::vnoise

struct FlameParams {
    // Eased throttle [0, 1]; 0 = hidden (the CPU also flips Visibility).
    strength: f32,
    // Raycast distance nozzle→ground along the exhaust axis, flame-local units.
    // Anything beyond the flame's reach (e.g. 1e6) disables the splash.
    ground_dist: f32,
    // Per-rocket noise phase so neighbouring flames never flicker in sync.
    phase: f32,
    // Plume length at full throttle, flame-local units.
    flame_len: f32,
    // Ground normal at the raycast hit, rotated into flame-local space (xyz).
    ground_normal: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> material: FlameParams;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // Normalized distance along the flame [0 nozzle → 1 tip]: the fade axis.
    @location(0) t: f32,
    // 0 in free air → 1 fully past the ground bend (the splash skirt).
    @location(1) splash: f32,
    // Angle around the exhaust axis; scrolls the turbulence around the plume.
    @location(2) ang: f32,
};

// Flame radius at the nozzle exit (the flare's exit radius is 0.8).
const NOZZLE_RADIUS: f32 = 0.55;
// The splash skirt spreads faster along the ground than the free plume flows.
const SPLASH_SPREAD: f32 = 1.6;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let time = globals.time;

    // Unit-cylinder parametrisation: y +0.5 = nozzle ring, -0.5 = tip.
    let t = clamp(0.5 - vertex.position.y, 0.0, 1.0);
    // This vertex's radial direction around the axis (cap-centre verts sit on
    // the axis; give them a stable dummy direction — their radius is ~0 anyway).
    let xz = vec2<f32>(vertex.position.x, vertex.position.z);
    var c = vec3<f32>(1.0, 0.0, 0.0);
    let xz_len = length(xz);
    if xz_len > 1e-4 {
        c = vec3<f32>(xz.x / xz_len, 0.0, xz.y / xz_len);
    }
    let ang = atan2(c.z, c.x);

    let strength = material.strength;
    // Plume length: mostly throttle, plus a slow whole-plume breathing flicker.
    let breathe = vnoise(vec2<f32>(material.phase * 17.0, time * 6.0)) - 0.5;
    let len = material.flame_len * (0.35 + 0.65 * strength) * (1.0 + 0.12 * breathe);

    // Bulge-then-taper profile, rippled by scrolling noise so the silhouette
    // licks instead of reading as a rigid cone.
    var r = NOZZLE_RADIUS * (1.0 + 0.55 * t) * pow(max(1.0 - t, 0.0), 0.55)
        * (0.75 + 0.25 * strength);
    let ripple = vnoise(vec2<f32>(ang * 1.9 + material.phase, t * 3.5 - time * 5.0)) - 0.5;
    r = r * (1.0 + 0.55 * ripple);

    // Along-flame distance, split at the ground hit.
    let s = t * len;
    let d = material.ground_dist;
    let s_air = min(s, d);
    let s_gnd = max(s - d, 0.0);
    // Blend factor into the splash skirt (soft over ~0.25 units of travel).
    let splash = clamp(s_gnd / 0.25, 0.0, 1.0);

    let axis = vec3<f32>(0.0, -1.0, 0.0);
    var pos = axis * s_air + c * r * (1.0 - 0.6 * splash);
    if s_gnd > 0.0 {
        let n = material.ground_normal.xyz;
        // Deflected flow: the axis' ground-tangential component (grazing hits
        // keep flowing "forward") topped up with this vertex's own radial
        // direction projected onto the plane (a square hit fans out evenly).
        let a_t = axis - dot(axis, n) * n;
        let r_t = c - dot(c, n) * n;
        var flow = a_t + r_t * (1.0 - min(length(a_t), 1.0));
        let flow_len = length(flow);
        if flow_len > 1e-4 {
            flow = flow / flow_len;
        } else {
            flow = r_t;
        }
        // Travel along the ground plane, hovering a little off it so the skirt
        // hugs the surface without z-fighting; noise fluffs it vertically.
        pos = pos + flow * s_gnd * SPLASH_SPREAD
            + n * splash * (0.07 + 0.3 * r * (0.5 + ripple));
    }

    out.clip_position = mesh_position_local_to_clip(
        get_world_from_local(vertex.instance_index),
        vec4<f32>(pos, 1.0),
    );
    out.t = t;
    out.splash = splash;
    out.ang = ang;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let time = globals.time;
    let t = in.t;

    // Two octaves of scrolling turbulence: a slow structural lick plus a fast
    // fine shimmer, both racing tipward (negative t direction as time grows).
    let n1 = vnoise(vec2<f32>(in.ang * 2.2 + material.phase * 13.0, t * 4.0 - time * 4.5));
    let n2 = vnoise(vec2<f32>(in.ang * 5.0 - time * 1.7, t * 9.0 - time * 9.0 + material.phase));
    let turbulence = 0.65 * n1 + 0.35 * n2;

    // White-hot core → orange body → deep-red tip; the splash reheats toward
    // yellow where the plume slams the ground.
    var col = mix(vec3<f32>(2.6, 2.3, 1.6), vec3<f32>(2.3, 1.05, 0.15), smoothstep(0.0, 0.45, t));
    col = mix(col, vec3<f32>(1.1, 0.18, 0.03), smoothstep(0.4, 1.0, t));
    col = mix(col, vec3<f32>(2.2, 1.3, 0.3), in.splash * 0.5);

    var alpha = material.strength * pow(max(1.0 - t, 0.0), 1.3) * (0.35 + 0.65 * turbulence);
    // Feather the very tip so the last ring never pops.
    alpha = alpha * smoothstep(0.0, 0.12, 1.0 - t);

    return vec4<f32>(col, alpha);
}
