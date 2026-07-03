use bevy::{
    asset::uuid_handle,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderType},
    shader::ShaderRef,
};

/// The procedural part-metal shader (`client/assets/metal_material.wgsl`),
/// embedded at compile time by `RenderMainPassPlugin` — see the WGSL header.
/// Like the grass, it extends `StandardMaterial`, perturbing only base colour
/// and roughness with brushed/flaked/scratched surface texture.
pub const METAL_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("b6e3d45f-acf3-4bad-8d9f-b85545fdb33a");

pub type MetalMaterial = ExtendedMaterial<StandardMaterial, MetalExtension>;

/// The part's surface finish. Discriminants match the `FINISH_*` consts in
/// the WGSL.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Finish {
    /// Linear grinding lines.
    Brushed = 0,
    /// Polished in circles: concentric rings around a per-part centre.
    Circular = 1,
    /// Hot-dip galvanized steel: splotchy zinc-crystal spangle (Voronoi cells
    /// with a random brightness per crystal + darkened boundaries).
    Galvanized = 2,
}

/// Mirrors `MetalParams` in the WGSL field-for-field. Everything here is
/// pre-randomized per part by [`metal_material`].
#[derive(ShaderType, Debug, Clone, Default)]
pub struct MetalParams {
    /// Brushing (machining) direction on each face, as cos/sin of the angle.
    brush_cos: f32,
    brush_sin: f32,
    /// Finish pattern density: streaks/rings across a face, or spangle
    /// crystals per face for galvanized.
    brush_freq: f32,
    /// Finish brightness/roughness modulation [0, 1]; ~0 = polished flat.
    brush_strength: f32,
    /// Sparkle-flake cell density.
    flake_freq: f32,
    /// Sparkle strength [0, 1]; 0 = none (most parts).
    flake_strength: f32,
    /// Scratch gouge strength [0, 1]; 0 = pristine.
    scratch_strength: f32,
    /// Offsets the noise domain so identical shapes never share a pattern.
    noise_offset: f32,
    /// Ring centre for the circular finish, in per-face UV space.
    center_x: f32,
    center_y: f32,
    /// `Finish` discriminant.
    finish: u32,
    _pad: u32,
}

#[derive(Asset, AsBindGroup, Debug, Clone, Default, TypePath)]
pub struct MetalExtension {
    #[uniform(100)]
    params: MetalParams,
}

impl MaterialExtension for MetalExtension {
    fn fragment_shader() -> ShaderRef {
        METAL_SHADER_HANDLE.into()
    }
}

/// splitmix64 step, folded to a uniform f32 in [0, 1). Hand-rolled (5 lines)
/// rather than a `rand` PRNG because the derivation must be *deterministic
/// across platforms and versions* — every multiplayer client (wasm + native)
/// derives the same look from the same replicated seed, and `rand` guarantees
/// neither cross-version nor cross-platform stability for its small RNGs.
fn next_unit(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
}

/// The part's finish and metal tint, derived from its seed alone — their own
/// stream, so the focus-highlight reset (`highlight_grabbable`, `highlight.rs`)
/// can recover a part's colour via [`metal_tint`] without rebuilding the whole
/// material. The finish is drawn *first* because it constrains the tint:
/// galvanized parts are always zinc-gray.
fn finish_and_tint(seed: u32) -> (Finish, Color) {
    // (weight, sRGB albedo) — real-metal base colours (standard PBR reference
    // values), weighted toward the common structural metals so most parts read
    // as steel/aluminum with the occasional copper, brass, or gold.
    const METALS: &[(f32, [f32; 3])] = &[
        (0.22, [0.56, 0.57, 0.58]), // steel
        (0.15, [0.35, 0.36, 0.38]), // gunmetal
        (0.14, [0.91, 0.92, 0.92]), // aluminum
        (0.10, [0.62, 0.58, 0.54]), // titanium
        (0.09, [0.66, 0.61, 0.53]), // nickel
        (0.10, [0.95, 0.64, 0.54]), // copper
        (0.09, [0.91, 0.78, 0.42]), // brass
        (0.07, [0.80, 0.58, 0.36]), // bronze
        (0.04, [1.00, 0.77, 0.34]), // gold
    ];
    let mut s = seed as u64;
    let finish = match next_unit(&mut s) {
        f if f < 0.4 => Finish::Brushed,
        f if f < 0.7 => Finish::Circular,
        _ => Finish::Galvanized,
    };
    let mut rgb = if finish == Finish::Galvanized {
        // Zinc coating: always the same faintly blue light gray, whatever the
        // base steel — the spangle brightness variation supplies the colour
        // interest ("splotchy" is the finish, not the tint).
        [0.72, 0.74, 0.77]
    } else {
        let mut pick = next_unit(&mut s);
        let mut chosen = METALS[0].1;
        for (weight, albedo) in METALS {
            chosen = *albedo;
            if pick < *weight {
                break;
            }
            pick -= weight;
        }
        chosen
    };
    // Small per-part brightness wiggle so two steel parts still differ.
    let l = 0.85 + next_unit(&mut s) * 0.3;
    for c in &mut rgb {
        *c = (*c * l).min(1.0);
    }
    (finish, Color::srgb(rgb[0], rgb[1], rgb[2]))
}

/// The part's colour alone (for the focus-highlight reset).
pub fn metal_tint(seed: u32) -> Color {
    finish_and_tint(seed).1
}

/// Derive a part's whole material from its spawn seed. Deterministic: same
/// seed → same look on every client and platform (see [`next_unit`]).
pub fn metal_material(seed: u32) -> MetalMaterial {
    let (finish, tint) = finish_and_tint(seed);
    // Texture params draw from an independent stream (seed XOR'd) so adding or
    // reordering draws here can never shift the tint stream above.
    let mut s = (seed ^ 0xA511_05ED) as u64;
    let mut range = |lo: f32, hi: f32| lo + next_unit(&mut s) * (hi - lo);

    let angle = range(0.0, std::f32::consts::TAU);
    let (brush_freq, brush_strength) = match finish {
        // Fine grinding lines; too coarse reads as wood grain.
        Finish::Brushed => (range(120.0, 300.0), range(0.05, 0.25)),
        // Rings still need to be fine — coarse rings on a warm tint read as
        // tree rings, not machining.
        Finish::Circular => (range(110.0, 240.0), range(0.06, 0.2)),
        // Spangle crystals per face; strong contrast is the whole look.
        Finish::Galvanized => (range(6.0, 16.0), range(0.25, 0.5)),
    };
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: tint,
            // Full `metallic: 1.0` goes near-black here — with no environment
            // map the only specular sources are the single sun + ambient — so
            // stay moderate: reads as anodized/painted metal that still glints.
            metallic: range(0.45, 0.85),
            perceptual_roughness: range(0.12, 0.4),
            ..Default::default()
        },
        extension: MetalExtension {
            params: MetalParams {
                brush_cos: angle.cos(),
                brush_sin: angle.sin(),
                brush_freq,
                brush_strength,
                flake_freq: range(120.0, 250.0),
                // Under half the parts sparkle at all.
                flake_strength: (range(-0.5, 0.5)).max(0.0),
                // Most parts are lightly worn; some are beat up.
                scratch_strength: (range(-0.3, 0.7)).max(0.0),
                noise_offset: range(0.0, 64.0),
                center_x: range(0.25, 0.75),
                center_y: range(0.25, 0.75),
                finish: finish as u32,
                _pad: 0,
            },
        },
    }
}
