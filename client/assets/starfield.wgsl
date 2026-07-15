// Star dome — a field of distant stars that fades in as the atmosphere thins with
// altitude and washes out again on the way back down (the reverse of the ash + haze).
//
// Like the ash field, this is entirely GPU-driven and camera-anchored: the CPU spawns
// ONE unit sphere mesh once and never touches it. The vertex shader ignores the mesh's
// own transform and places every vertex on a fixed-radius dome around the *camera*
// (`view.world_position + dir * STAR_RADIUS`), so the stars sit at a constant apparent
// distance no matter where the ship flies — a skybox with no cubemap. The dome radius is
// kept inside the camera's far plane and beyond the near scene, so the ground still
// occludes the stars below the horizon (they only show in open sky) while nothing near
// pokes through. Rendered transparent (depth-tested, no depth write) and unlit; the whole
// effect is one draw call and a couple of uniforms.

#import bevy_pbr::mesh_view_bindings::{view, globals}

// x = visibility 0..1 (0 in thick low air, 1 in clear space); yzw padding.
@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> star: vec4<f32>;

// Apparent star distance (m). Inside the default far plane, past the ground horizon.
const STAR_RADIUS: f32 = 900.0;

struct Vertex {
    @location(0) position: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // World-space view direction for this vertex (interpolated, re-normalised in frag).
    @location(0) dir: vec3<f32>,
};

// Dave Hoskins' hash33 (https://www.shadertoy.com/view/4djSRW, CC0): a 3-vector seed to
// three decorrelated randoms in [0,1).
fn hash33(p: vec3<f32>) -> vec3<f32> {
    var p3 = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yxz + 33.33);
    return fract((p3.xxy + p3.yzz) * p3.zyx);
}

const TAU: f32 = 6.2831853;

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    let dir = normalize(v.position);
    let world = view.world_position + dir * STAR_RADIUS;
    var out: VertexOutput;
    out.clip_position = view.clip_from_world * vec4<f32>(world, 1.0);
    out.dir = dir;
    return out;
}

// Sparse stars from a view direction: parametrise the sphere by longitude/latitude,
// grid it, and drop at most one star per cell. Cheap (single cell, no neighbour loop) —
// the mild distortion near the poles is invisible in a random field.
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
    // Star position inside the cell, its size, and a slow independent twinkle.
    let center = vec2<f32>(0.2 + 0.6 * h.x, 0.2 + 0.6 * h.y);
    let d = length(f - center);
    let core = smoothstep(0.09, 0.0, d);
    let twinkle = 0.6 + 0.4 * sin(globals.time * (0.8 + h.x * 2.0) + h.y * TAU);
    // Faint colour variation: mostly white, a few warm/blue.
    let tint = mix(vec3<f32>(0.75, 0.82, 1.0), vec3<f32>(1.0, 0.85, 0.7), h.x);
    return tint * core * twinkle;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let visibility = star.x;
    if visibility <= 0.001 {
        discard;
    }
    let c = starfield(normalize(in.dir)) * visibility;
    let a = max(max(c.r, c.g), c.b);
    if a < 0.004 {
        discard;
    }
    return vec4<f32>(c, a);
}
