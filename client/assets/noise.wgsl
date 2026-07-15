// Shared procedural-noise library for the game's material shaders (grass turf,
// part metal). Embedded at compile time and imported via
// `#import bad_spaceship::noise::{...}` — never fetched at runtime.
//
// Ported from tuxalin/procedural-tileable-shaders (github, MIT): `ihash1D`
// (Hugo Elias' integer hash) + `betterHash2D` + the quintic-interpolated value
// noise, minus the domain tiling (world space never wraps). The GLSL hashes
// cast the position straight to uint, which is undefined for negative
// coordinates (half our platform lives at negative XZ) — this port goes
// through i32 first (two's-complement bitcast, well-defined in WGSL).

#define_import_path bad_spaceship::noise

// Hugo Elias' integer hash, 4 lanes (tuxalin `ihash1D`, MIT).
fn ihash1d(q0: vec4<u32>) -> vec4<u32> {
    var q = q0 * 747796405u + 2891336453u;
    q = (q << vec4(13u)) ^ q;
    return q * (q * q * 15731u + 789221u) + 1376312589u;
}

// One random value per cell corner (tuxalin `betterHash2D(vec4)`, MIT).
// `cell` is (x0, y0, x1, y1); returns hashes of (x0,y0),(x1,y0),(x0,y1),(x1,y1).
fn hash_corners(cell: vec4<f32>) -> vec4<f32> {
    let i = bitcast<vec4<u32>>(vec4<i32>(floor(cell)));
    let h = ihash1d(ihash1d(i.xzxz) + i.yyww);
    return vec4<f32>(h) * (1.0 / 4294967295.0);
}

// 2D value noise, quintic-interpolated. Range [0, 1].
fn vnoise(pos: vec2<f32>) -> f32 {
    let ip = floor(pos);
    let f = pos - ip;
    let h = hash_corners(vec4(ip, ip + 1.0));
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return mix(mix(h.x, h.y, u.x), mix(h.z, h.w, u.x), u.y);
}

// Two-octave FBM — stands in for an authored noise texture.
fn fbm(pos: vec2<f32>) -> f32 {
    return vnoise(pos) * 0.6667 + vnoise(pos * 2.0 + vec2(37.2, 17.7)) * 0.3333;
}

// Dave Hoskins' hash33 (https://www.shadertoy.com/view/4djSRW, CC0): a 3-vector seed to
// three decorrelated randoms in [0,1). The 3D sibling of the ash shader's `hash31` —
// kept here so the Hoskins family lives in the one shared library.
fn hash33(p: vec3<f32>) -> vec3<f32> {
    var p3 = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yxz + 33.33);
    return fract((p3.xxy + p3.yzz) * p3.zyx);
}
