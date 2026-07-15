// The sky — every pixel of it, at every altitude, from one physical model.
//
// A camera-anchored dome pinned to the far plane: for each sky direction it computes
// the atmosphere's transmittance T along the infinite ray (bad_spaceship::atmosphere)
// and shows `smog·(1−T) + stars·T`. That single integral IS the whole sky:
//   * on the pad, T ≈ 0 everywhere → a red smog sky, thickest toward the horizon;
//   * climbing, the zenith clears first (shortest column) → stars pierce overhead
//     while the horizon is still a wall — and the transition is continuous, because
//     there is nothing to switch, only path lengths shrinking;
//   * from orbit, rays grazing the planet take the longest chords → the smog ring
//     hugging the limb with its soft halo, while clear directions show steady stars.
//
// Geometry occludes the dome (it's depth-tested at the far plane), so the planet
// blocks the stars behind it for free. Like the ash field, the CPU spawns ONE mesh
// and never touches it; the only per-frame input is the floating-origin offset.

#import bevy_pbr::mesh_view_bindings::view
#import bad_spaceship::atmosphere::{transmittance, FOG_RGB}
#import bad_spaceship::noise::hash33

// The room's visual floating-origin offset (xyz; w padding): camera TRUE position =
// render position + this, so the integral sees real altitude during a rebased ascent.
@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> sky_frame_offset: vec4<f32>;

struct Vertex {
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // World-space view direction for this vertex (interpolated, re-normalised in frag).
    @location(0) dir: vec3<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    let dir = normalize(v.position);
    // Project the direction at infinity (w = 0 drops the camera translation — the
    // standard skybox projection), then pin the depth to the far plane (reverse-Z:
    // far = 0) so EVERY piece of scene geometry — the planet included — occludes the
    // sky behind it.
    let clip = view.clip_from_world * vec4<f32>(dir, 0.0);
    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip.xy, 0.0, clip.w);
    out.dir = dir;
    return out;
}

// Sparse steady stars from a view direction: parametrise the sphere by longitude/
// latitude, grid it, and drop at most one star per cell. Cheap (single cell, no
// neighbour loop) — the mild distortion near the poles is invisible in a random field.
fn starfield(dir: vec3<f32>) -> vec3<f32> {
    let lon = atan2(dir.z, dir.x);
    let lat = asin(clamp(dir.y, -1.0, 1.0));
    let p = vec2<f32>(lon, lat) * vec2<f32>(64.0, 40.0);
    let cell = floor(p);
    let f = fract(p);
    let h = hash33(vec3<f32>(cell, 7.0));
    // Only the sparse cells (h.z high) hold a star.
    if h.z < 0.86 {
        return vec3<f32>(0.0);
    }
    let center = vec2<f32>(0.2 + 0.6 * h.x, 0.2 + 0.6 * h.y);
    let core = smoothstep(0.09, 0.0, length(f - center));
    // Faint colour variation: mostly white, a few warm/blue.
    let tint = mix(vec3<f32>(0.75, 0.82, 1.0), vec3<f32>(1.0, 0.85, 0.7), h.x);
    return tint * core;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.dir);
    let cam_true = view.world_position + sky_frame_offset.xyz;
    let t = transmittance(cam_true, dir, 1.0e9);
    // Deep in the smog (the whole sky at pad level) the stars contribute nothing —
    // skip their trig/hash entirely. Warp-coherent: neighbouring fragments share the
    // regime, so the branch genuinely saves the work.
    if t < 0.001 {
        return vec4<f32>(FOG_RGB, 1.0);
    }
    return vec4<f32>(starfield(dir) * t + FOG_RGB * (1.0 - t), 1.0);
}
