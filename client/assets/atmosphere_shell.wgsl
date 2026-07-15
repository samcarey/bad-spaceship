// Planet atmosphere shell — a smog layer hugging the planet: thick at the limb, thinning
// to a faint wash over the disc, with a soft halo just beyond the edge. It's what you see
// looking back at the planet from orbit (the ash field is camera-centred and can't play
// this role — this is anchored to the planet).
//
// Rendered on a sphere at the atmosphere's OUTER radius, parented under the ground so it
// co-moves with the floating origin. Each fragment analytically integrates the view ray's
// chord through the atmosphere annulus (impact-parameter form): the smog piles up where
// the sightline grazes the dense lower air (the limb) and is thin where it punches
// straight down through the disc. Cheap — no marching, a couple of sqrts.
//
// Back-face culled, so it only draws when the camera is OUTSIDE the shell (in space);
// down inside the atmosphere the DistanceFog carries the haze instead.

#import bevy_pbr::{
    mesh_view_bindings::view,
    forward_io::VertexOutput,
}

struct AtmosphereShell {
    // rgb = smog tint; a = overall intensity.
    color: vec4<f32>,
    // x = planet surface radius, y = atmosphere outer radius, z = density scale, w unused.
    params: vec4<f32>,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> atmo: AtmosphereShell;

// Softening band (fraction of surface radius²) across the planet-edge step, where the
// far side of the chord snaps from blocked (inside the disc) to visible (the halo).
const LIMB_SOFT: f32 = 0.03;

// Altitude (m above the shell's outer radius) over which the whole shell fades in as the
// camera climbs out of the atmosphere. Back-face culling already hides the shell while
// you're inside it; without this ramp it would snap fully on the instant you cross the
// boundary. Fading over a few km makes it emerge smoothly as you rise and look back.
const APPEAR_BAND: f32 = 3000.0;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let world = in.world_position.xyz;
    let n = normalize(in.world_normal);
    let r_surf = atmo.params.x;
    let r_top = atmo.params.y;

    // The shell is a sphere of radius r_top; recover its centre from this fragment.
    let center = world - n * r_top;
    let cam = view.world_position;
    let dir = normalize(world - cam);
    let oc = center - cam;
    let tca = dot(oc, dir);
    let b2 = dot(oc, oc) - tca * tca; // squared impact parameter of the view ray

    let rt2 = r_top * r_top;
    if b2 >= rt2 {
        discard; // ray misses the atmosphere entirely
    }
    let outer_half = sqrt(rt2 - b2);
    let rs2 = r_surf * r_surf;
    let surf_half = sqrt(max(rs2 - b2, 0.0));

    // Inside the planet disc the far side is blocked by the planet → only the near
    // atmosphere segment (outer shell down to the surface) counts. Outside the disc the
    // ray passes clean through the annulus → the full chord, both sides. Blend across the
    // limb so the planet edge doesn't read as a hard ring.
    let near_path = outer_half - surf_half;
    let full_path = 2.0 * outer_half;
    let t = smoothstep(rs2 * (1.0 - LIMB_SOFT), rs2 * (1.0 + LIMB_SOFT), b2);
    let path = mix(near_path, full_path, t);

    // Fade the whole shell in as the camera climbs above the atmosphere, so it emerges
    // smoothly on the way out instead of snapping on at the boundary.
    let cam_dist = length(cam - center);
    let appear = smoothstep(r_top, r_top + APPEAR_BAND, cam_dist);

    let alpha = clamp(path * atmo.params.z, 0.0, 1.0) * atmo.color.a * appear;
    return vec4<f32>(atmo.color.rgb, alpha);
}
