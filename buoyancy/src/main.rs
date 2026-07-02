//! Buoyancy simulator — a standalone single-player physics experiment.
//!
//! A translucent-blue body of water sits in the middle of the screen; a conical
//! frustum (a cylinder with independently adjustable end radii) spawns in the air
//! and drops in. Each frame the buoyant force plus gravity, drag, and other
//! optional hydrodynamic effects are applied through Avian's `Forces` helper, so
//! it bobs, tips, rights itself, and settles in real time.
//!
//! The buoyancy is computed by one of three selectable methods (radio buttons in
//! the bottom panel), each with a real-time quality slider, all built for
//! arbitrary closed geometry rather than exploiting the frustum's symmetry:
//!
//! 1. **Voxel grid** — the body is voxelised in local space (grid-density
//!    slider); each cell below the surface contributes ρ·g·V_cell upward at its
//!    centre, with fractional submersion at the waterline band. The approach
//!    Unity's Boat Attack demo and voxel-construction games (Stormworks) use.
//! 2. **Surface pressure** — the hull triangle mesh is clipped against the water
//!    plane and hydrostatic pressure ρ·g·depth is integrated per submerged
//!    triangle (Kerner's Just Cause 3 boat model). The only method with honest
//!    per-face dynamic forces, so the pressure-drag and slamming add-ons attach
//!    here.
//! 3. **Clipped volume** — the exact submerged volume and centre of buoyancy of
//!    the clipped mesh via the divergence theorem, applied as a single force
//!    (the Jolt Physics approach; zero sampling noise, the reference answer).
//!
//! Optional add-on force models (checkboxes): viscous drag (all methods, applied
//! per-element where the method has elements), pressure drag and slamming
//! (surface-pressure method only).
//!
//! This is a SEPARATE crate + WASM bundle from the main game — it shares no game
//! code, only the workspace engine version pins.

mod hydro;

use avian3d::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::{MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::light::GlobalAmbientLight;
use bevy::mesh::Indices;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;
use bevy_egui::{egui, input::EguiWantsInput, EguiContexts, EguiPlugin, EguiPrimaryContextPass};

// ── Scene / physics constants ────────────────────────────────────────────────
/// Water surface height (world Y). Buoyancy is computed against this plane.
const WATER_LEVEL: f32 = 0.0;
/// Horizontal extent of the (square) water body and floor.
const WATER_SIZE: f32 = 20.0;
/// Depth of the visible water volume (surface at `WATER_LEVEL`, floor below).
const WATER_DEPTH: f32 = 6.0;

/// Density of water (kg/m³) — the reference the slider compares against.
const WATER_DENSITY: f32 = 1000.0;
/// Gravitational acceleration (matches Avian's default `Gravity`).
const GRAVITY: f32 = 9.81;
/// Kinematic viscosity of water (m²/s), for the ITTC skin-friction line.
const WATER_VISCOSITY: f32 = 1.0e-6;

/// Reset drop height (body centre Y in the air, above the water).
const SPAWN_HEIGHT: f32 = 5.0;

/// Starting shell-material density: steel. The body is a hollow sealed can,
/// so even in steel it floats (a 1 cm-wall default shell displaces ~5× its
/// mass); crank the wall thickness to sink it.
const DEFAULT_DENSITY: f32 = 7850.0;
/// Density slider range — up past lead (11340) so heavy metals fit.
const DENSITY_RANGE: std::ops::RangeInclusive<f32> = 100.0..=12000.0;

// Shape slider defaults/ranges. The body is a conical frustum: a cylinder when
// the two end radii match, a cone-ish shape when they don't.
const DEFAULT_RADIUS: f32 = 1.2;
const DEFAULT_LENGTH: f32 = 3.2;
const RADIUS_RANGE: std::ops::RangeInclusive<f32> = 0.2..=2.5;
const LENGTH_RANGE: std::ops::RangeInclusive<f32> = 0.5..=6.4;
/// Hollow-shell wall thickness slider default/range, in cm (the UI unit).
const DEFAULT_WALL_CM: f32 = 1.0;
const WALL_CM_RANGE: std::ops::RangeInclusive<f32> = 0.1..=25.0;

/// Opacity of the water volume.
const WATER_ALPHA: f32 = 0.13125;
/// Opacity of the body's shell — double the water's, so the hollow interior
/// (and the force vectors inside it) read through the walls.
const BODY_ALPHA: f32 = WATER_ALPHA * 2.0;

// ── Shape (conical frustum) ──────────────────────────────────────────────────
// The body's local axis is +Y: "side 1" is the top face (at +length/2), "side 2"
// the bottom face. The buoyancy methods consume the shape only through
// `frustum_contains` (voxeliser) and the closed triangle mesh (hydro.rs), so
// swapping in truly arbitrary geometry later means replacing just these helpers.

/// The frustum's shape parameters. One value that the mesh builders, the
/// collider, and both method caches key on — a future shape field flows into
/// every signature and cache key by construction instead of by grep.
#[derive(Clone, Copy, PartialEq)]
struct Shape {
    /// Radius of side 1 (the +Y / top face).
    r_top: f32,
    /// Radius of side 2 (the -Y / bottom face).
    r_bottom: f32,
    length: f32,
}

/// Point-in-shape test in the body's local frame.
fn frustum_contains(shape: Shape, p: Vec3) -> bool {
    if p.y.abs() > shape.length * 0.5 {
        return false;
    }
    let t = p.y / shape.length + 0.5;
    let r = shape.r_bottom + (shape.r_top - shape.r_bottom) * t;
    p.x * p.x + p.z * p.z <= r * r
}

/// The internal cavity of the hollow shell: the outer frustum inset by the
/// wall thickness, radially and at both ends. `None` when the walls meet and
/// the body is effectively solid.
fn inner_shape(shape: Shape, wall: f32) -> Option<Shape> {
    let inner = Shape {
        r_top: shape.r_top - wall,
        r_bottom: shape.r_bottom - wall,
        length: shape.length - 2.0 * wall,
    };
    (inner.r_top > 1e-4 && inner.r_bottom > 1e-4 && inner.length > 1e-4).then_some(inner)
}

/// Mass, angular inertia, and centre of mass of the hollow body: the outer
/// frustum minus the internal cavity, both evaluated at the material density.
/// `MassProperties3d - MassProperties3d` does the compound-body subtraction
/// (mass-weighted COM, parallel-axis-shifted tensors), so the shell gets a
/// true thin-wall inertia tensor (larger per unit mass than a solid's).
///
/// The buoyancy methods keep using the sealed *outer* hull — a closed shell
/// displaces its full outer volume; hollowness only changes how mass is
/// distributed.
fn shell_mass_props(shape: Shape, wall: f32, density: f32) -> (Mass, AngularInertia, CenterOfMass) {
    let outer = *ColliderMassProperties::from_shape(&frustum_collider(shape), density);
    let props = match inner_shape(shape, wall) {
        Some(cavity) => outer - *ColliderMassProperties::from_shape(&frustum_collider(cavity), density),
        None => outer,
    };
    (
        Mass(props.mass),
        AngularInertia::from_tensor(props.angular_inertia_tensor()),
        CenterOfMass(props.center_of_mass),
    )
}

/// Render/collider tessellation resolution around the axis.
const SHAPE_SEGMENTS: usize = 32;

/// Render mesh for the frustum's lateral surface only (no end caps — the ends
/// are separate `Circle` discs so they can carry a lighter material; Bevy's
/// built-in `ConicalFrustum` mesh is one-piece, hence the hand-roll). Smooth
/// radial normals, tilted by the wall's slope.
fn frustum_side_mesh(shape: Shape) -> Mesh {
    let n = SHAPE_SEGMENTS;
    let half = shape.length * 0.5;
    let slope = (shape.r_bottom - shape.r_top) / shape.length;
    let mut positions = Vec::with_capacity(2 * (n + 1));
    let mut normals = Vec::with_capacity(2 * (n + 1));
    for i in 0..=n {
        positions.push(hydro::ring_point(i, n, half, shape.r_top).to_array());
        positions.push(hydro::ring_point(i, n, -half, shape.r_bottom).to_array());
        // Unit ring point = (cos a, 0, sin a): the radial direction of the normal.
        let radial = hydro::ring_point(i, n, 0.0, 1.0);
        let normal = Vec3::new(radial.x, slope, radial.z).normalize().to_array();
        normals.push(normal);
        normals.push(normal);
    }
    let mut indices = Vec::with_capacity(n * 6);
    for i in 0..n as u32 {
        let (t0, b0, t1, b1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        indices.extend([t0, t1, b0, b0, t1, b1]); // outward winding
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_indices(Indices::U32(indices))
}

/// Convex-hull collider from the two end rims.
fn frustum_collider(shape: Shape) -> Collider {
    let n = SHAPE_SEGMENTS;
    let half = shape.length * 0.5;
    let mut points = Vec::with_capacity(2 * n);
    for i in 0..n {
        points.push(hydro::ring_point(i, n, half, shape.r_top));
        points.push(hydro::ring_point(i, n, -half, shape.r_bottom));
    }
    Collider::convex_hull(points).expect("frustum rim points form a valid hull")
}

// ── Components / resources ───────────────────────────────────────────────────
/// The one buoyant body.
#[derive(Component)]
struct Buoy;

/// The body's render pieces (children of the `Buoy` entity): the lateral wall
/// and the two end discs, split so the ends can be a lighter shade.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum BuoyPart {
    Side,
    /// The cavity wall — the inner surface of the hollow shell, visible
    /// through the translucent outer wall. Hidden when the body is solid.
    InnerSide,
    /// End disc at +length/2 (side 1) or -length/2 (side 2): `sign` = ±1.
    Cap {
        sign: i8,
    },
}

/// Which buoyancy computation drives the body (the radio selector).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    VoxelGrid,
    SurfacePressure,
    ClippedVolume,
}

/// Live-tunable simulation parameters (driven by the egui panel).
#[derive(Resource)]
struct SimParams {
    density: f32,
    /// Starting tilt of the body's centre axis off vertical, degrees (applied on Reset).
    start_angle_deg: f32,
    shape: Shape,
    /// Hollow-shell wall thickness, in cm (the slider's unit).
    wall_cm: f32,

    method: Method,
    /// Voxel method quality: cells along the body's longest local dimension.
    voxel_res: u32,
    /// Mesh methods quality: radial segments of the hull tessellation.
    hull_res: u32,

    /// Viscous drag add-on (all methods; per-element where the method has them).
    drag_on: bool,
    drag_coeff: f32,
    /// Pressure drag add-on (surface-pressure method): quadratic normal-flow drag.
    pressure_drag_on: bool,
    pressure_drag_coeff: f32,
    /// Slamming add-on (surface-pressure method): water-entry impact force.
    slamming_on: bool,
    /// Max "violence" Γ_max (m/s²) at which the slamming force saturates.
    gamma_max: f32,
}

impl SimParams {
    fn hull_rings(&self) -> usize {
        (self.hull_res as usize / 3).max(2)
    }

    /// Shell wall thickness in metres.
    fn wall(&self) -> f32 {
        self.wall_cm * 0.01
    }
}

impl Default for SimParams {
    fn default() -> Self {
        Self {
            density: DEFAULT_DENSITY,
            start_angle_deg: 0.0,
            shape: Shape {
                r_top: DEFAULT_RADIUS,
                r_bottom: DEFAULT_RADIUS,
                length: DEFAULT_LENGTH,
            },
            wall_cm: DEFAULT_WALL_CM,
            method: Method::VoxelGrid,
            voxel_res: 12,
            hull_res: 24,
            drag_on: true,
            drag_coeff: 1.0,
            pressure_drag_on: true,
            pressure_drag_coeff: 1.0,
            slamming_on: false,
            gamma_max: 20.0,
        }
    }
}

/// Set by the UI's Reset button; consumed by `handle_reset`.
#[derive(Resource, Default)]
struct ResetRequested(bool);

/// The force-vector overlay: every hydro force applied this frame, as
/// (application point, force) pairs in world space. Collected by
/// `apply_hydro_forces` when `on`, drawn as gizmo arrows by
/// `draw_force_vectors` — so what you see is exactly what was applied,
/// per element, whatever the method (voxel centres, triangle centroids,
/// or the single centre-of-buoyancy vector).
#[derive(Resource)]
struct ForceVectors {
    on: bool,
    arrows: Vec<(Vec3, Vec3)>,
}

impl Default for ForceVectors {
    fn default() -> Self {
        Self {
            on: true,
            arrows: Vec::new(),
        }
    }
}

/// Longest drawn arrow (m). Lengths are normalised to the frame's largest
/// force so they stay proportional to each other at any scale — per-voxel
/// forces and the clipped-volume method's one total-buoyancy vector differ
/// by orders of magnitude.
const MAX_ARROW_LEN: f32 = 1.5;
const ARROW_COLOR: Color = Color::srgb(1.0, 0.45, 0.1);

/// Cost of the per-frame hydro-force computation, averaged over ~1 s windows
/// for the overlay readout. Written by `apply_hydro_forces`, read by `ui`.
#[derive(Resource, Default)]
struct PhysicsTiming {
    /// Seconds of compute accumulated in the current window.
    accum: f32,
    frames: u32,
    /// Wall-clock seconds elapsed in the current window.
    window: f32,
    /// Last completed window's per-frame average (ms) — the displayed value.
    avg_ms: f32,
}

/// Cached voxelisation of the body in LOCAL space: occupied cell centres + the
/// per-cell volume. Rebuilt only when the shape or grid resolution changes; the
/// per-frame work is just transforming centres to world space.
#[derive(Resource, Default)]
struct VoxelCache {
    /// (shape, resolution) the cache was built for.
    key: Option<(Shape, u32)>,
    centers: Vec<Vec3>,
    cell_size: f32,
    cell_volume: f32,
}

/// Cached hull tessellation in LOCAL space for the mesh-based methods, plus
/// per-triangle data the slamming model needs. Same rebuild policy as `VoxelCache`.
#[derive(Resource, Default)]
struct HullCache {
    key: Option<(Shape, u32)>,
    tris: Vec<[Vec3; 3]>,
    /// Local-space geometry per triangle (`None` = degenerate), cached so the
    /// slamming pass doesn't recompute cross products of constant triangles.
    tri_geoms: Vec<Option<hydro::TriGeom>>,
    /// Total surface area (for slamming's per-triangle share of the stopping force).
    total_area: f32,
    /// Slamming state: last frame's swept-volume rate per triangle.
    prev_dv: Vec<f32>,
}

/// Orbit-camera state: yaw/pitch around the body's centre, `distance` back from
/// it. Written by `orbit_input` (drag = orbit, scroll/pinch = zoom), applied to
/// the camera transform by `update_camera` (which follows the frustum's live
/// position, so the view stays centred while it bobs and drifts).
#[derive(Resource)]
struct OrbitCamera {
    yaw: f32,
    pitch: f32, // elevation above the horizon, rad
    distance: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        // Camera pitched down 20° (= 20° elevation above the horizon here),
        // yawed 30° off-axis, at the default distance.
        Self {
            yaw: 30f32.to_radians(),
            pitch: 20f32.to_radians(),
            distance: DEFAULT_ZOOM,
        }
    }
}

const ORBIT_SENS: f32 = 0.005; // rad per drag pixel
const PITCH_RANGE: std::ops::RangeInclusive<f32> =
    -std::f32::consts::FRAC_PI_4..=1.5; // -45°..~86°
const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 6.0..=72.0;
/// Starting camera distance (the pre-widened zoom-out limit, not the new max).
const DEFAULT_ZOOM: f32 = 60.0;
/// How far (in NDC units, i.e. fractions of *half* the screen height ×2) the
/// scene centre should sit above the viewport centre, so that it lands midway
/// between the top of the screen and the top of the bottom control panel.
/// That midpoint is `panel_height / 2` above the visible centre, which in NDC is
/// exactly `panel_height / screen_height`. Measured by `ui` (the panel height is
/// dynamic — it wraps on narrow screens), applied by `update_camera`.
#[derive(Resource, Default)]
struct ScreenLift(f32);

#[bevy_main]
fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Buoyancy Simulator".to_string(),
                    ..default()
                }),
                ..default()
            }),
        )
        .add_plugins(PhysicsPlugins::default())
        .add_plugins(EguiPlugin::default())
        .insert_resource(ClearColor(Color::srgb(0.99, 0.99, 0.95)))
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 400.0,
            ..default()
        })
        .init_resource::<SimParams>()
        .init_resource::<ResetRequested>()
        .init_resource::<ForceVectors>()
        .init_resource::<PhysicsTiming>()
        .init_resource::<VoxelCache>()
        .init_resource::<HullCache>()
        .init_resource::<OrbitCamera>()
        .init_resource::<ScreenLift>()
        .add_systems(Startup, setup)
        // Shape rebuild → reset (repositions) → mass props → buoyancy (writes Forces).
        // Chained so the frame's writes to mesh/collider/velocity/mass are deterministic.
        .add_systems(
            Update,
            (
                rebuild_shape,
                handle_reset,
                sync_mass_props,
                apply_hydro_forces,
                draw_force_vectors,
            )
                .chain(),
        )
        .add_systems(Update, (orbit_input, update_camera).chain())
        // Fonts must land before the first `ui` frame — with `default_fonts`
        // off, egui panics on any text until definitions are installed.
        .add_systems(
            EguiPrimaryContextPass,
            (install_fonts.run_if(run_once), ui).chain(),
        )
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    params: Res<SimParams>,
) {
    // Camera: looks down at the centre of the water from an angle so the surface
    // fills most of the viewport. The pose is owned by `OrbitCamera` (drag to
    // orbit, scroll/pinch to zoom); `update_camera` writes it every frame, so the
    // spawn transform is just a placeholder for frame 0.
    commands.spawn((
        Camera3d::default(),
        // Bevy's default TonyMcMapface tonemapping needs the `tonemapping_luts`
        // feature (KTX2 LUT assets) which this slim build doesn't pull; without it
        // every material renders magenta/pink on wasm. Reinhard needs no LUT —
        // same choice the game makes (shared/src/player.rs).
        Tonemapping::ReinhardLuminance,
        Transform::from_xyz(0.0, 9.0, 14.0).looking_at(Vec3::new(0.0, -1.0, 0.0), Vec3::Y),
    ));

    // Sun.
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(6.0, 12.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Water — a translucent blue volume (surface at `WATER_LEVEL`, extending down).
    // Visual only: the body passes through the surface and is acted on by the
    // buoyancy force, so the water carries no collider.
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(WATER_SIZE, WATER_DEPTH, WATER_SIZE))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgba(0.1, 0.45, 0.85, WATER_ALPHA),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.1,
            ..default()
        })),
        Transform::from_xyz(0.0, WATER_LEVEL - WATER_DEPTH * 0.5, 0.0),
    ));

    // Floor at the bottom of the water so a dense (sinking) body comes to rest
    // on it rather than falling out of view. Static collider + a grey slab.
    let floor_top = WATER_LEVEL - WATER_DEPTH;
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(WATER_SIZE, 1.0, WATER_SIZE))),
        MeshMaterial3d(materials.add(Color::srgb(0.45, 0.45, 0.45))),
        Transform::from_xyz(0.0, floor_top - 0.5, 0.0),
        RigidBody::Static,
        Collider::cuboid(WATER_SIZE, 1.0, WATER_SIZE),
    ));

    // The buoyant body: a hollow, sealed shell. Explicit `Mass`/`AngularInertia`/
    // `CenterOfMass` (from `shell_mass_props`) override Avian's collider-derived
    // solid values, so the body weighs and rotates like a thin-walled can while
    // the collider (and the buoyancy hulls) stay the sealed outer frustum. The
    // visuals are translucent children: the outer wall and end discs in army
    // green, plus the cavity's inner wall so the hollowness reads. Both
    // materials render back faces (`cull_mode: None`) so the shell is visible
    // through itself.
    let shape = params.shape;
    let mut shell_material = |color| {
        materials.add(StandardMaterial {
            base_color: color,
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.6,
            double_sided: true,
            cull_mode: None,
            ..default()
        })
    };
    let side_material = shell_material(Color::srgba(0.29, 0.33, 0.13, BODY_ALPHA)); // army green
    let cap_material = shell_material(Color::srgba(0.36, 0.40, 0.18, BODY_ALPHA)); // lighter green
    let (mass, inertia, com) = shell_mass_props(shape, params.wall(), params.density);
    commands
        .spawn((
            Buoy,
            Transform::from_xyz(0.0, SPAWN_HEIGHT, 0.0),
            Visibility::default(),
            RigidBody::Dynamic,
            frustum_collider(shape),
            mass,
            inertia,
            com,
        ))
        .with_children(|body| {
            body.spawn((
                BuoyPart::Side,
                Mesh3d(meshes.add(frustum_side_mesh(shape))),
                MeshMaterial3d(side_material.clone()),
            ));
            // Placeholder mesh, hidden: `rebuild_shape`'s first run (its cache
            // starts empty) swaps in the real cavity mesh and visibility, so
            // the solid-vs-hollow rule lives only there.
            body.spawn((
                BuoyPart::InnerSide,
                Mesh3d(meshes.add(frustum_side_mesh(shape))),
                MeshMaterial3d(side_material),
                Visibility::Hidden,
            ));
            for sign in [1i8, -1] {
                body.spawn((
                    BuoyPart::Cap { sign },
                    Mesh3d(meshes.add(cap_mesh(shape, sign))),
                    MeshMaterial3d(cap_material.clone()),
                    cap_transform(shape, sign),
                ));
            }
        });
}

/// End-disc mesh for one cap: side 1 (+Y) or side 2 (-Y).
fn cap_mesh(shape: Shape, sign: i8) -> Mesh {
    let r = if sign > 0 { shape.r_top } else { shape.r_bottom };
    Circle::new(r)
        .mesh()
        .resolution(SHAPE_SEGMENTS as u32)
        .build()
}

/// Pose for one end disc: at ±length/2, facing outward along ±Y (the `Circle`
/// disc faces +Z, so rotate ∓90° about X).
fn cap_transform(shape: Shape, sign: i8) -> Transform {
    let half = shape.length * 0.5;
    Transform::from_xyz(0.0, sign as f32 * half, 0.0).with_rotation(Quat::from_rotation_x(
        -(sign as f32) * std::f32::consts::FRAC_PI_2,
    ))
}

/// Rebuild the render meshes + collider when a shape or wall slider moves.
/// (Mass properties are explicit overrides synced by `sync_mass_props`, so the
/// collider swap only affects collision geometry.)
fn rebuild_shape(
    params: Res<SimParams>,
    mut cache: Local<Option<(Shape, f32)>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut bodies: Query<&mut Collider, With<Buoy>>,
    mut parts: Query<(&BuoyPart, &mut Mesh3d, &mut Transform, &mut Visibility)>,
) {
    // Cache key uses the raw slider fields (`wall_cm`, like `sync_mass_props`)
    // so the two systems' keys stay directly comparable.
    let (shape, wall) = (params.shape, params.wall());
    if *cache == Some((shape, params.wall_cm)) {
        return;
    }
    *cache = Some((shape, params.wall_cm));
    for mut collider in &mut bodies {
        *collider = frustum_collider(shape);
    }
    for (part, mut mesh, mut transform, mut visibility) in &mut parts {
        let new_mesh = match *part {
            BuoyPart::Side => frustum_side_mesh(shape),
            BuoyPart::InnerSide => match inner_shape(shape, wall) {
                Some(cavity) => {
                    *visibility = Visibility::Inherited;
                    frustum_side_mesh(cavity)
                }
                // Solid: hide the inner wall (the stale mesh is invisible).
                None => {
                    *visibility = Visibility::Hidden;
                    continue;
                }
            },
            BuoyPart::Cap { sign } => {
                *transform = cap_transform(shape, sign);
                cap_mesh(shape, sign)
            }
        };
        let old = mesh.0.clone();
        mesh.0 = meshes.add(new_mesh);
        meshes.remove(&old);
    }
}

/// Recompute the shell's explicit mass properties when density, shape, or wall
/// thickness change (they override Avian's collider-derived solid values).
fn sync_mass_props(
    params: Res<SimParams>,
    mut cache: Local<Option<(Shape, f32, f32)>>,
    mut bodies: Query<(&mut Mass, &mut AngularInertia, &mut CenterOfMass), With<Buoy>>,
) {
    let key = (params.shape, params.wall_cm, params.density);
    if *cache == Some(key) {
        return;
    }
    *cache = Some(key);
    let (mass, inertia, com) = shell_mass_props(params.shape, params.wall(), params.density);
    for (mut m, mut i, mut c) in &mut bodies {
        *m = mass;
        *i = inertia;
        *c = com;
    }
}

/// On a Reset request, lift the body back into the air at rest — tilted by the
/// start-angle slider — to drop again.
fn handle_reset(
    mut reset: ResMut<ResetRequested>,
    params: Res<SimParams>,
    mut bodies: Query<
        (
            &mut Position,
            &mut Rotation,
            &mut LinearVelocity,
            &mut AngularVelocity,
            &mut Transform,
        ),
        With<Buoy>,
    >,
) {
    if !reset.0 {
        return;
    }
    reset.0 = false;
    let tilt = Quat::from_rotation_z(params.start_angle_deg.to_radians());
    // `Transform` is written alongside the physics pose deliberately: Avian's
    // Position→Transform sync runs with the physics step, so without it the
    // render pose would lag the reset by up to a frame.
    for (mut pos, mut rot, mut vel, mut ang, mut transform) in &mut bodies {
        pos.0 = Vec3::new(0.0, SPAWN_HEIGHT, 0.0);
        rot.0 = tilt;
        vel.0 = Vec3::ZERO;
        ang.0 = Vec3::ZERO;
        transform.translation = pos.0;
        transform.rotation = tilt;
    }
}

/// (Re)voxelise the body in local space when the shape or resolution changes.
fn refresh_voxel_cache(params: &SimParams, cache: &mut VoxelCache) {
    let key = (params.shape, params.voxel_res);
    if cache.key == Some(key) {
        return;
    }
    cache.key = Some(key);
    cache.centers.clear();

    let shape = params.shape;
    let r_max = shape.r_top.max(shape.r_bottom);
    let extent = Vec3::new(2.0 * r_max, shape.length, 2.0 * r_max);
    let cell = extent.max_element() / params.voxel_res as f32;
    cache.cell_size = cell;
    cache.cell_volume = cell * cell * cell;

    let counts = (extent / cell).ceil().as_ivec3().max(IVec3::ONE);
    let origin = -Vec3::new(
        counts.x as f32 * cell,
        counts.y as f32 * cell,
        counts.z as f32 * cell,
    ) * 0.5;
    for ix in 0..counts.x {
        for iy in 0..counts.y {
            for iz in 0..counts.z {
                let c = origin
                    + cell * Vec3::new(ix as f32 + 0.5, iy as f32 + 0.5, iz as f32 + 0.5);
                if frustum_contains(shape, c) {
                    cache.centers.push(c);
                }
            }
        }
    }
}

/// (Re)tessellate the hull in local space when the shape or resolution changes.
fn refresh_hull_cache(params: &SimParams, cache: &mut HullCache) {
    let key = (params.shape, params.hull_res);
    if cache.key == Some(key) {
        return;
    }
    cache.key = Some(key);
    cache.tris = hydro::frustum_triangles(
        params.shape.r_top,
        params.shape.r_bottom,
        params.shape.length,
        params.hull_res as usize,
        params.hull_rings(),
    );
    cache.tri_geoms = cache.tris.iter().map(hydro::tri_geom).collect();
    cache.total_area = cache.tri_geoms.iter().flatten().map(|g| g.area).sum();
    // Triangle identity changed — old swept-volume history is meaningless.
    cache.prev_dv.clear();
    cache.prev_dv.resize(cache.tris.len(), 0.0);
}

/// The active buoyancy method + enabled add-ons, applied through Avian's
/// `Forces` helper (gravity comes from Avian itself via the body's mass).
/// All distributed forces go through `apply_force_at_point`, which also
/// generates the correct torque about the centre of mass — righting and
/// capsizing moments emerge without any explicit torque model.
fn apply_hydro_forces(
    time: Res<Time>,
    params: Res<SimParams>,
    mut voxels: ResMut<VoxelCache>,
    mut hull: ResMut<HullCache>,
    // Reused frame-to-frame so clipping never allocates in steady state.
    mut clipped: Local<Vec<[Vec3; 3]>>,
    mut timing: ResMut<PhysicsTiming>,
    mut vis: ResMut<ForceVectors>,
    mut bodies: Query<(&Position, &Rotation, &ComputedMass, Forces), With<Buoy>>,
) {
    let started = Instant::now();
    let dt = time.delta_secs().max(1e-4);
    vis.arrows.clear();
    let record = vis.on;

    for (pos, rot, mass, mut forces) in &mut bodies {
        // Thousands of points get transformed per frame: pre-expand the
        // quaternion to a matrix, and keep its world-Y row separate so dry
        // elements can be rejected on a one-component transform.
        let rot_m = Mat3::from_quat(rot.0);
        let row_y = Vec3::new(rot_m.x_axis.y, rot_m.y_axis.y, rot_m.z_axis.y);
        let world_y = |local: Vec3| pos.0.y + row_y.dot(local);

        match params.method {
            // ── 1. Voxel grid ────────────────────────────────────────────────
            Method::VoxelGrid => {
                refresh_voxel_cache(&params, &mut voxels);
                for local in &voxels.centers {
                    // Fractional submersion of the cell (treated as `cell_size`
                    // tall — exact for full/empty cells, softened at the
                    // waterline band, which is the standard anti-jitter fix).
                    let frac = ((WATER_LEVEL - world_y(*local)) / voxels.cell_size + 0.5)
                        .clamp(0.0, 1.0);
                    if frac <= 0.0 {
                        continue;
                    }
                    let world = pos.0 + rot_m * *local;
                    let v_cell = voxels.cell_volume * frac;
                    let mut f = Vec3::Y * (WATER_DENSITY * GRAVITY * v_cell);
                    if params.drag_on {
                        // Per-cell linear drag on the point velocity; the offset
                        // application makes angular damping fall out for free.
                        f += -3.0 * params.drag_coeff * WATER_DENSITY * v_cell
                            * forces.velocity_at_point(world);
                    }
                    forces.apply_force_at_point(f, world);
                    if record {
                        vis.arrows.push((world, f));
                    }
                }
            }

            // ── 2. Surface pressure integration (Kerner) ─────────────────────
            Method::SurfacePressure => {
                refresh_hull_cache(&params, &mut hull);
                let hull = &mut *hull; // allow disjoint field borrows in the loop

                // ITTC 1957 skin-friction coefficient, once per body per frame.
                let speed = forces.linear_velocity().length();
                let reynolds = (speed * params.shape.length / WATER_VISCOSITY).max(1e3);
                let c_f = 0.075 / (reynolds.log10() - 2.0).powi(2);

                for (j, tri) in hull.tris.iter().enumerate() {
                    // Dry triangle: no forces; just clear its slamming history.
                    if tri.iter().all(|v| world_y(*v) > WATER_LEVEL) {
                        hull.prev_dv[j] = 0.0;
                        continue;
                    }
                    let world = hydro::tri_to_world(tri, pos.0, &rot_m);
                    clipped.clear();
                    hydro::clip_triangle_below(world, WATER_LEVEL, &mut clipped);

                    let mut sub_area = 0.0;
                    for sub in clipped.iter() {
                        let Some(g) = hydro::tri_geom(sub) else { continue };
                        sub_area += g.area;
                        let depth = WATER_LEVEL - g.centroid.y;
                        // Hydrostatic pressure force. Only the vertical component
                        // is kept: the horizontal parts cancel exactly over a
                        // closed surface, and dropping them kills numerical
                        // drift (verified: the vertical-only torque matches the
                        // exact centre-of-buoyancy torque to <0.5%).
                        let f_y = -(WATER_DENSITY * GRAVITY * depth * g.area) * g.normal.y;
                        let mut f = Vec3::new(0.0, f_y, 0.0);

                        let v_p = forces.velocity_at_point(g.centroid);
                        if params.drag_on {
                            // Viscous water resistance: skin friction on the
                            // tangential flow (ITTC line).
                            let v_t = v_p - g.normal * v_p.dot(g.normal);
                            f += -(0.5 * WATER_DENSITY * c_f * g.area * v_t.length())
                                * v_t
                                * params.drag_coeff;
                        }
                        if params.pressure_drag_on {
                            // Quadratic normal-flow drag/suction: resists motion
                            // into the water (v·n̂ > 0) and pulls when the face
                            // retreats (v·n̂ < 0) — Kerner's pressure drag in a
                            // ρ-scaled, sign-symmetric form.
                            let v_n = v_p.dot(g.normal);
                            f += -(0.5 * WATER_DENSITY * g.area * v_n * v_n.abs())
                                * g.normal
                                * params.pressure_drag_coeff;
                        }
                        forces.apply_force_at_point(f, g.centroid);
                        if record {
                            vis.arrows.push((g.centroid, f));
                        }
                    }

                    if !params.slamming_on {
                        hull.prev_dv[j] = 0.0;
                        continue;
                    }
                    // Kerner's slamming model, per ORIGINAL hull triangle:
                    // Γ = rate of change of the swept-volume rate, normalised by
                    // the triangle's area; the stopping force saturates at Γ_max
                    // and is capped at "this triangle's share of stopping the
                    // whole body in one frame".
                    let Some(g) = &hull.tri_geoms[j] else { continue };
                    let center = pos.0 + rot_m * g.centroid;
                    let v_p = forces.velocity_at_point(center);
                    let speed_p = v_p.length();
                    let dv = sub_area * speed_p;
                    let gamma = (dv - hull.prev_dv[j]).abs() / (g.area * dt);
                    hull.prev_dv[j] = dv;
                    if speed_p > 1e-4 {
                        let cos_theta = (v_p / speed_p).dot(rot_m * g.normal);
                        if cos_theta > 0.0 && gamma > 0.0 {
                            let f_stop =
                                mass.value() * speed_p * (2.0 * g.area / hull.total_area);
                            let scale = (gamma / params.gamma_max).clamp(0.0, 1.0).powi(2);
                            let f_slam = -(scale * cos_theta * f_stop / speed_p) * v_p;
                            forces.apply_force_at_point(f_slam, center);
                            if record {
                                vis.arrows.push((center, f_slam));
                            }
                        }
                    }
                }
            }

            // ── 3. Exact clipped volume + centre of buoyancy (Jolt-style) ────
            Method::ClippedVolume => {
                refresh_hull_cache(&params, &mut hull);
                clipped.clear();
                for tri in &hull.tris {
                    if tri.iter().all(|v| world_y(*v) > WATER_LEVEL) {
                        continue;
                    }
                    hydro::clip_triangle_below(
                        hydro::tri_to_world(tri, pos.0, &rot_m),
                        WATER_LEVEL,
                        &mut clipped,
                    );
                }
                if let Some((v_sub, center_of_buoyancy)) =
                    hydro::submerged_volume_centroid(&clipped, WATER_LEVEL)
                {
                    let f_buoy = Vec3::Y * (WATER_DENSITY * GRAVITY * v_sub);
                    forces.apply_force_at_point(f_buoy, center_of_buoyancy);
                    if record {
                        vis.arrows.push((center_of_buoyancy, f_buoy));
                    }
                    if params.drag_on {
                        // Body-level drag at the centre of buoyancy (this method
                        // has no per-element sites): quadratic + linear on the
                        // linear velocity, plus Jolt-style angular damping.
                        let v_lin = forces.linear_velocity();
                        let a_proj = v_sub.powf(2.0 / 3.0);
                        let f = -(0.5 * WATER_DENSITY * 0.5 * a_proj * v_lin.length()
                            + 1.5 * WATER_DENSITY * v_sub)
                            * v_lin
                            * params.drag_coeff;
                        forces.apply_force_at_point(f, center_of_buoyancy);
                        if record {
                            vis.arrows.push((center_of_buoyancy, f));
                        }
                        let torque = -(0.02
                            * WATER_DENSITY
                            * v_sub
                            * params.shape.length
                            * params.shape.length
                            * params.drag_coeff)
                            * forces.angular_velocity();
                        forces.apply_torque(torque);
                    }
                }
            }
        }
    }

    timing.accum += started.elapsed().as_secs_f32();
    timing.frames += 1;
    timing.window += time.delta_secs();
    if timing.window >= 1.0 {
        timing.avg_ms = timing.accum / timing.frames.max(1) as f32 * 1000.0;
        timing.accum = 0.0;
        timing.frames = 0;
        timing.window = 0.0;
    }
}

/// Draw this frame's applied forces as gizmo arrows. Lengths are proportional
/// to force magnitude, normalised so the frame's largest force spans
/// `MAX_ARROW_LEN` (see the constant for why absolute scaling doesn't work).
fn draw_force_vectors(vis: Res<ForceVectors>, mut gizmos: Gizmos) {
    let max = vis
        .arrows
        .iter()
        .map(|(_, f)| f.length())
        .fold(0.0f32, f32::max);
    if max <= 0.0 {
        return;
    }
    let scale = MAX_ARROW_LEN / max;
    for (point, force) in &vis.arrows {
        let tip = *point + *force * scale;
        // Skip near-zero arrows: a degenerate gizmo arrow still draws its head.
        if point.distance_squared(tip) > 1e-6 {
            gizmos.arrow(*point, tip, ARROW_COLOR);
        }
    }
}

/// Camera gestures: drag (mouse left / one finger) orbits yaw+pitch around the
/// tank; scroll wheel (desktop) / two-finger pinch (mobile) zooms. Skipped while
/// egui wants the pointer so panel interactions don't also move the camera.
fn orbit_input(
    mut orbit: ResMut<OrbitCamera>,
    egui_wants: Res<EguiWantsInput>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    touches: Res<Touches>,
) {
    if egui_wants.wants_any_pointer_input() {
        motion.clear();
        wheel.clear();
        return;
    }

    let active: Vec<_> = touches.iter().collect();
    let mut drag = Vec2::ZERO;
    if active.len() >= 2 {
        // Pinch zoom: scale distance by the ratio of the finger gap last frame
        // to this frame (spread → gap grows → zoom in).
        let (a, b) = (active[0], active[1]);
        let cur = a.position().distance(b.position());
        let prev = a.previous_position().distance(b.previous_position());
        if cur > 1.0 && prev > 1.0 {
            orbit.distance =
                (orbit.distance * prev / cur).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
        }
    } else if let Some(touch) = active.first() {
        drag = touch.delta();
    } else if buttons.pressed(MouseButton::Left) {
        for m in motion.read() {
            drag += m.delta;
        }
    }
    orbit.yaw -= drag.x * ORBIT_SENS;
    orbit.pitch =
        (orbit.pitch + drag.y * ORBIT_SENS).clamp(*PITCH_RANGE.start(), *PITCH_RANGE.end());

    for w in wheel.read() {
        // Normalise pixel-based wheels (trackpads) to notch-ish units.
        let lines = match w.unit {
            MouseScrollUnit::Line => w.y,
            MouseScrollUnit::Pixel => w.y / 60.0,
        };
        orbit.distance =
            (orbit.distance * 0.92f32.powf(lines)).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
    }
    motion.clear();
}

/// Apply the orbit state to the camera transform, centred on the body.
fn update_camera(
    orbit: Res<OrbitCamera>,
    lift: Res<ScreenLift>,
    body: Query<&Transform, (With<Buoy>, Without<Camera3d>)>,
    mut cameras: Query<(&mut Transform, &Projection), With<Camera3d>>,
) {
    let Ok((mut transform, projection)) = cameras.single_mut() else {
        return;
    };
    let target = body.single().map_or(Vec3::ZERO, |t| t.translation);
    let rotation = Quat::from_rotation_y(orbit.yaw) * Quat::from_rotation_x(-orbit.pitch);
    transform.translation = target + rotation * (Vec3::Z * orbit.distance);
    // The offset is `rotation * +Z` and the camera looks along its local -Z, so
    // `rotation` IS the orientation facing the target (no roll) — no look_at.
    transform.rotation = rotation;
    // Tilt down so the scene centre rides higher in the viewport (see
    // `ScreenLift`): an NDC shift of `s` needs atan(s · tan(fov/2)) of tilt.
    // Screen-space constant, so orbit/zoom gestures are unaffected.
    let half_fov = match projection {
        Projection::Perspective(p) => p.fov * 0.5,
        _ => std::f32::consts::FRAC_PI_4 * 0.5,
    };
    transform.rotate_local_x(-(lift.0 * half_fov.tan()).atan());
}

// `window.location.href`, for resolving same-site links to absolute URLs:
// egui's open-url backend (`webbrowser`) only accepts absolute http(s) URLs,
// so a relative link would silently do nothing on click.
#[wasm_bindgen::prelude::wasm_bindgen(
    inline_js = "export function page_href() { return window.location.href; }"
)]
extern "C" {
    fn page_href() -> String;
}

/// Absolute URL of the "how it works" page, resolved once against the page
/// location (works under both the root-served site and a sub-path deploy).
fn info_url() -> &'static str {
    static URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    URL.get_or_init(|| {
        let href = page_href();
        let base = href.split(['?', '#']).next().unwrap_or(&href);
        let dir_end = base.rfind('/').map(|i| i + 1).unwrap_or(base.len());
        format!("{}buoyancy/info.html", &base[..dir_end])
    })
}

/// UI zoom (egui 0.34 per-context zoom factor).
const UI_ZOOM: f32 = 1.3;
/// Fixed width of the label column to the left of each slider, in points.
const SLIDER_LABEL_WIDTH: f32 = 120.0;
/// Room reserved to the right of each slider for its value box.
const SLIDER_VALUE_WIDTH: f32 = 80.0;

/// A slider row: label on the left (fixed-width column so the sliders line up),
/// slider stretched over the rest of the row's width.
fn labeled_slider(ui: &mut egui::Ui, label: &str, slider: egui::Slider) {
    ui.horizontal(|ui| {
        let r = ui.label(label);
        ui.add_space((SLIDER_LABEL_WIDTH - r.rect.width()).max(0.0));
        ui.spacing_mut().slider_width = (ui.available_width() - SLIDER_VALUE_WIDTH).max(60.0);
        ui.add(slider);
    });
}

/// Minimal font set replacing egui's 1.4 MB `default_fonts` (disabled in
/// Cargo.toml — see the bevy_egui dep comment): a Latin subset of Ubuntu-Light
/// (ASCII + ° ± ² ³ · × — …) and a two-glyph emoji-icon subset (⟲ 👁).
/// If the UI gains a new glyph, regenerate with pyftsubset and verify coverage
/// by parsing the ttf cmap first — a missing glyph renders as a blank box.
fn subset_fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert(
        "ubuntu-subset".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/ubuntu-light-subset.ttf")).into(),
    );
    fonts.font_data.insert(
        "icons-subset".to_owned(),
        egui::FontData::from_static(include_bytes!("../fonts/emoji-icon-subset.ttf"))
            // Same tweak egui's defaults give emoji-icon-font ("bigger emojis").
            .tweak(egui::FontTweak {
                scale: 0.90,
                ..Default::default()
            })
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.insert(
            family,
            vec!["ubuntu-subset".to_owned(), "icons-subset".to_owned()],
        );
    }
    fonts
}

/// One-shot (`run_once`): install the subset fonts on the egui context.
fn install_fonts(mut contexts: EguiContexts) -> Result {
    contexts.ctx_mut()?.set_fonts(subset_fonts());
    Ok(())
}

/// A frameless overlay `Area` pinned to a screen corner. Free Areas remember
/// last frame's width and wrap text to it — a shrink feedback loop that ends
/// one character per line — so the style is forced to never wrap.
fn floating_area(
    ctx: &egui::Context,
    id: &str,
    align: egui::Align2,
    offset: [f32; 2],
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Area::new(egui::Id::new(id))
        .anchor(align, offset)
        .show(ctx, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            add_contents(ui);
        });
}

/// Bottom control panel: shape + density sliders, method radio + quality,
/// add-on checkboxes, reset.
fn ui(
    mut contexts: EguiContexts,
    mut params: ResMut<SimParams>,
    mut reset: ResMut<ResetRequested>,
    mut lift: ResMut<ScreenLift>,
    timing: Res<PhysicsTiming>,
    mut vis: ResMut<ForceVectors>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    if (ctx.zoom_factor() - UI_ZOOM).abs() > 0.01 {
        ctx.set_zoom_factor(UI_ZOOM);
    }
    // Floating "source" permalink, top-right over the sim — a frameless Area, so
    // no background. Links to this crate's directory in the GitHub repo at the
    // exact commit this binary was built from (`BUILD_COMMIT`, from build.rs).
    floating_area(ctx, "source-link", egui::Align2::RIGHT_TOP, [-10.0, 10.0], |ui| {
        ui.hyperlink_to(
            "</> source",
            concat!(
                "https://github.com/samcarey/bad-spaceship/tree/",
                env!("BUILD_COMMIT"),
                "/buoyancy"
            ),
        );
    });
    // Floating "info" link, top-left — opens the rendered how-it-works page
    // (buoyancy/info.html) explaining every force calculation with equations.
    floating_area(ctx, "info-link", egui::Align2::LEFT_TOP, [10.0, 10.0], |ui| {
        ui.hyperlink_to("how it works", info_url());
    });
    // Panel frame: the egui default side margin (8) widened by 50%.
    let frame = egui::Frame::side_top_panel(&ctx.global_style()).inner_margin(egui::Margin {
        left: 12,
        right: 12,
        top: 2,
        bottom: 2,
    });
    // Bottom panel. (`TopBottomPanel::bottom` + `show(ctx, ..)` are deprecated in
    // egui 0.34 in favour of `Panel` + `show_inside`, but the replacement nests
    // inside a `Ui` — same deferred deprecation the game's ui.rs carries.)
    #[allow(deprecated)]
    let panel = egui::TopBottomPanel::bottom("controls")
        .frame(frame)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("⟲  Reset (drop again)").clicked() {
                    reset.0 = true;
                }
                ui.separator();
                ui.label(format!("water: {WATER_DENSITY:.0} kg/m³"));
            });
            ui.add_space(4.0);
            labeled_slider(
                ui,
                "body density",
                egui::Slider::new(&mut params.density, DENSITY_RANGE)
                    .logarithmic(true)
                    .suffix(" kg/m³"),
            );
            labeled_slider(
                ui,
                "start angle",
                egui::Slider::new(&mut params.start_angle_deg, 0.0..=90.0).suffix("°"),
            );
            egui::CollapsingHeader::new("Cylinder")
                .default_open(true)
                .show(ui, |ui| {
                    labeled_slider(
                        ui,
                        "radius, side 1",
                        egui::Slider::new(&mut params.shape.r_top, RADIUS_RANGE).suffix(" m"),
                    );
                    labeled_slider(
                        ui,
                        "radius, side 2",
                        egui::Slider::new(&mut params.shape.r_bottom, RADIUS_RANGE).suffix(" m"),
                    );
                    labeled_slider(
                        ui,
                        "length",
                        egui::Slider::new(&mut params.shape.length, LENGTH_RANGE).suffix(" m"),
                    );
                    labeled_slider(
                        ui,
                        "wall thickness",
                        egui::Slider::new(&mut params.wall_cm, WALL_CM_RANGE)
                            .logarithmic(true)
                            .suffix(" cm"),
                    );
                });

            ui.separator();
            // Collapsed by default: the method selector + its quality slider +
            // the add-on force models are tuning details, not the everyday knobs.
            egui::CollapsingHeader::new("Method").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    // Selectable labels (highlight when active), tightly packed,
                    // each with a thin outline.
                    ui.spacing_mut().item_spacing.x = 3.0;
                    // (`noninteractive.bg_stroke` is the theme's visible separator gray;
                    // the `inactive` widget stroke is transparent by default.)
                    let outline =
                        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
                    for (value, text) in [
                        (Method::VoxelGrid, "voxel grid"),
                        (Method::SurfacePressure, "surface press."),
                        (Method::ClippedVolume, "clipped vol."),
                    ] {
                        egui::Frame::new()
                            .stroke(outline)
                            .corner_radius(4)
                            .inner_margin(egui::Margin::symmetric(2, 1))
                            .show(ui, |ui| {
                                ui.selectable_value(&mut params.method, value, text);
                            });
                    }
                });
                // The active method's quality slider.
                match params.method {
                    Method::VoxelGrid => {
                        labeled_slider(
                            ui,
                            "grid density",
                            egui::Slider::new(&mut params.voxel_res, 4..=32),
                        );
                    }
                    Method::SurfacePressure | Method::ClippedVolume => {
                        labeled_slider(
                            ui,
                            "hull resolution",
                            egui::Slider::new(&mut params.hull_res, 8..=64),
                        );
                    }
                }

                // Optional add-on force models. Coefficient sliders appear when the
                // add-on is enabled (the checkbox doubles as their label); the
                // pressure-drag / slamming add-ons need the per-face data only the
                // surface-pressure method produces.
                let per_face = params.method == Method::SurfacePressure;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().slider_width = 90.0;
                    ui.checkbox(&mut params.drag_on, "viscous drag");
                    if params.drag_on {
                        ui.add(egui::Slider::new(&mut params.drag_coeff, 0.0..=3.0));
                    }
                    ui.separator();
                    ui.add_enabled(
                        per_face,
                        egui::Checkbox::new(&mut params.slamming_on, "slamming"),
                    );
                    if per_face && params.slamming_on {
                        ui.add(
                            egui::Slider::new(&mut params.gamma_max, 5.0..=100.0).suffix(" m/s²"),
                        );
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().slider_width = 90.0;
                    ui.add_enabled(
                        per_face,
                        egui::Checkbox::new(&mut params.pressure_drag_on, "pressure drag"),
                    );
                    if per_face && params.pressure_drag_on {
                        ui.add(egui::Slider::new(&mut params.pressure_drag_coeff, 0.0..=3.0));
                    }
                });
            });
            ui.add_space(6.0);
        });
    // Physics-cost readout, floating just above the panel's top edge.
    let above_panel = panel.response.rect.height() + 6.0;
    floating_area(
        ctx,
        "physics-timing",
        egui::Align2::LEFT_BOTTOM,
        [10.0, -above_panel],
        |ui| {
            ui.label(format!("physics: {:.2} ms", timing.avg_ms));
        },
    );
    // Force-vector overlay toggle, floating above the panel's right edge —
    // fill/outline stripped so only the text shows, dimmed while off.
    floating_area(
        ctx,
        "vectors-toggle",
        egui::Align2::RIGHT_BOTTOM,
        [-10.0, -above_panel],
        |ui| {
            // On: the arrows' orange, so the button visibly binds to what it
            // draws; off: dimmed + struck through. (A `.weak()` dim alone was
            // indistinguishable from on at this size.)
            let text = if vis.on {
                egui::RichText::new("👁 vectors").color(egui::Color32::from_rgb(255, 115, 26))
            } else {
                egui::RichText::new("👁 vectors").weak().strikethrough()
            };
            let button = egui::Button::new(text)
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE);
            if ui.add(button).clicked() {
                vis.on = !vis.on;
            }
        },
    );
    // Centre the scene in the region *above* the panel: NDC shift =
    // panel_height / screen_height (see `ScreenLift`). Both rects are in egui
    // logical points, so the ratio is DPI-independent.
    lift.0 = panel.response.rect.height() / ctx.viewport_rect().height().max(1.0);
    Ok(())
}
