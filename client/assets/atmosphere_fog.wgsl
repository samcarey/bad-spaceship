// Physically-based atmosphere transmittance, shared by every shader that fogs.
//
// ONE model for ALL altitudes: the atmosphere is a density profile ρ(altitude) around
// the planet (the same renormalised exponential as `map::atmosphere_fraction` — the
// profile the drag physics flies through), and what any sightline sees is its optical
// depth, τ = extinction · ∫ ρ dl along the ray, giving transmittance T = e^(−τ).
// Everything the game needs falls out of that single integral with no special cases:
//   * standing on the pad, rays toward the horizon rack up τ fast → a smog wall, while
//     the zenith column is finite → the sky brightens (and stars first pierce) overhead;
//   * climbing, every path shortens smoothly → the world clears with altitude, no
//     boundary to pop across;
//   * from orbit, rays to the surface carry the full column (the surface blurs into
//     glow), rays grazing the limb take the longest chords (the smog ring), and rays
//     missing the planet show clean stars.
//
// Constants mirror `shared/src/map.rs` (and `client/src/render_main_pass/atmosphere.rs`
// for EXTINCTION / FOG_RGB) — keep them in lockstep, same discipline as the param
// structs mirrored field-for-field.

#define_import_path bad_spaceship::atmosphere

// Planet centre in TRUE world coordinates (map::PLANET_CENTER) — callers fold their
// floating-origin frame offset into positions before calling, exactly like gravity.
const CENTER: vec3<f32> = vec3<f32>(0.0, -15020.0, 0.0);
// Radius of the platform play surface (map::GRAVITY_REF_RADIUS): altitude zero.
const R_SURFACE: f32 = 15020.0;
// Altitude where the air reaches exactly zero (map::ATMOSPHERE_TOP_ALT).
const TOP: f32 = 4000.0;
// e-folding height of the density profile (map::ATMOSPHERE_SCALE_HEIGHT).
const H: f32 = 2000.0;
// Extinction coefficient at surface density (m⁻¹) — THE opacity knob (atmosphere.rs
// mirrors it for the near-field DistanceFog). τ = 1 per ~140 m of surface-density air.
const EXTINCTION: f32 = 0.007;
// What saturated smog looks like (linear-space): warm ember red — the lava-lit haze.
// Mirrors FOG_COLOR in atmosphere.rs (srgb 0.55, 0.20, 0.11).
const FOG_RGB: vec3<f32> = vec3<f32>(0.2633, 0.0331, 0.0116);

// Integration samples along the in-atmosphere segment. The profile is smooth (scale
// height 2 km) and the longest possible chord ~23 km, so midpoint sampling every
// ~1.5 km is plenty.
const SAMPLES: i32 = 16;

// Air density as a fraction of surface density at a TRUE world position — the exact
// mirror of `map::atmosphere_fraction`: 1 at/below the surface, exponential falloff
// renormalised to hit exactly 0 at TOP.
fn density_frac(p: vec3<f32>) -> f32 {
    let alt = length(p - CENTER) - R_SURFACE;
    if alt <= 0.0 {
        return 1.0;
    }
    if alt >= TOP {
        return 0.0;
    }
    let e_top = exp(-TOP / H); // constant-folds
    return (exp(-alt / H) - e_top) / (1.0 - e_top);
}

// Transmittance e^(−τ) along the ray `cam + t·dir` for t ∈ [0, max_dist], with the
// integral clipped to the atmosphere sphere (radius R_SURFACE + TOP) — rays that never
// enter the air return 1 without sampling. `cam` is the TRUE camera position (frame
// offset folded in); `dir` unit; pass a huge `max_dist` (e.g. 1e9) for sky rays.
fn transmittance(cam: vec3<f32>, dir: vec3<f32>, max_dist: f32) -> f32 {
    let r_top = R_SURFACE + TOP;
    let oc = CENTER - cam;
    let tca = dot(oc, dir);
    let b2 = dot(oc, oc) - tca * tca; // squared impact parameter
    let rt2 = r_top * r_top;
    if b2 >= rt2 {
        return 1.0; // misses the atmosphere entirely
    }
    let half = sqrt(rt2 - b2);
    let t0 = max(tca - half, 0.0);
    let t1 = min(tca + half, max_dist);
    if t1 <= t0 {
        return 1.0; // atmosphere lies behind the camera or beyond the target
    }
    let dt = (t1 - t0) / f32(SAMPLES);
    var tau = 0.0;
    for (var i = 0; i < SAMPLES; i++) {
        tau += density_frac(cam + dir * (t0 + (f32(i) + 0.5) * dt));
    }
    return exp(-tau * EXTINCTION * dt);
}

// Fog a lit surface colour by a transmittance: what survives of the surface plus what
// the smog radiates in its place.
fn fog_radiance(color: vec3<f32>, t: f32) -> vec3<f32> {
    return color * t + FOG_RGB * (1.0 - t);
}
