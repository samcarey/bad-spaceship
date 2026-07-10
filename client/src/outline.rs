//! Screen-space silhouette outline for grabbable / held parts.
//!
//! Replaces the old "tint the whole part yellow" focus highlight with a crisp
//! yellow line hugging the part's on-screen silhouette — a constant-pixel-width 2D
//! perimeter that is never occluded by the ground the part rests on.
//!
//! It works in screen space (the only way to get a truly uniform line — a vertex
//! "inverted-hull" shell has width that collapses on grazing edges and gets eaten
//! by the ground):
//! 1. Each outlined part gets a child proxy carrying a copy of its mesh on
//!    `RenderLayers(1)` (a solid unlit material).
//! 2. A second camera (`MaskCam`) sees ONLY layer 1 and renders those proxies to an
//!    offscreen coverage mask, kept in lock-step with the main camera's view.
//! 3. A full-screen post-process pass (`Core3dSystems::PostProcess` on the main
//!    camera) dilates the mask by a fixed pixel radius and composites the outline
//!    colour where the dilated mask sticks out past the silhouette — on top of the
//!    scene, so the ground can't hide it.
//!
//! Driven mode-agnostically off the local player's `FocusedInteractable` (set in
//! both modes: SP `update_focused`, MP `update_focus`).

use crate::render_main_pass::flame_material::FlameMaterial;
use bad_spaceship_shared::{FocusedInteractable, Player};
use bevy_egui::PrimaryEguiContext;
use bevy::{
    asset::{load_internal_asset, uuid_handle},
    camera::{visibility::RenderLayers, ClearColorConfig, RenderTarget},
    core_pipeline::{
        schedule::Core3d, tonemapping::Tonemapping, Core3dSystems, FullscreenShader,
    },
    image::Image,
    light::NotShadowCaster,
    prelude::*,
    render::{
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_asset::RenderAssets,
        render_resource::{
            binding_types::{sampler, texture_2d, uniform_buffer},
            BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendState,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FragmentState, LoadOp,
            Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor,
            RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
            ShaderType, StoreOp, TextureFormat, TextureSampleType,
        },
        renderer::{RenderContext, RenderDevice, ViewQuery},
        texture::GpuImage,
        view::{Msaa, ViewTarget},
        RenderApp, RenderStartup,
    },
    window::PrimaryWindow,
};

const OUTLINE_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("5f7a1c2e-3b4d-4e5f-9a1b-2c3d4e5f6a7b");

/// Render layer the mask camera renders (outlined-part proxies live here).
const MASK_LAYER: usize = 1;

/// Outline colour (bright yellow) and thickness in screen pixels. Tunable in one
/// place; feel-test the width (the mask is in physical pixels, so a high-DPR phone
/// makes a fixed count read thinner).
const OUTLINE_COLOR: Vec4 = Vec4::new(1.0, 0.95, 0.1, 0.75);
const OUTLINE_WIDTH_PX: f32 = 6.0;

pub struct OutlinePlugin;

impl Plugin for OutlinePlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            OUTLINE_SHADER_HANDLE,
            "../assets/outline_post.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins((
            ExtractResourcePlugin::<OutlineMaskImage>::default(),
            ExtractComponentPlugin::<OutlineSettings>::default(),
            UniformComponentPlugin::<OutlineSettings>::default(),
        ))
        .add_systems(Startup, setup_mask_resources)
        .add_systems(
            Update,
            (
                spawn_mask_camera,
                sync_outline_focus,
                spawn_outlines,
                despawn_outlines,
                resize_mask_image,
                toggle_outline_pass,
            ),
        )
        .add_systems(PostUpdate, sync_mask_camera.after(TransformSystems::Propagate));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .add_systems(RenderStartup, init_outline_pipeline)
            .add_systems(Core3d, outline_pass.in_set(Core3dSystems::PostProcess));
    }
}

// ---------------------------------------------------------------------------
// Main-world state
// ---------------------------------------------------------------------------

/// Marker: put on any entity with a `Mesh3d` to outline it; remove to clear.
#[derive(Component)]
pub struct Outlined;

/// Links an outlined part to the mask proxies it spawned (one per non-flame mesh in
/// its subtree — e.g. a rocket's body + flare), so they can be despawned when
/// [`Outlined`] is removed.
#[derive(Component)]
struct OutlineChild(Vec<Entity>);

/// The mask camera (renders outlined-part proxies to the coverage mask).
#[derive(Component)]
struct MaskCam;

/// Handle to the coverage-mask render target. Extracted to the render world so the
/// composite pass can bind its texture.
#[derive(Resource, Clone, ExtractResource)]
struct OutlineMaskImage(Handle<Image>);

/// Shared solid material for the mask proxies (unlit white → opaque coverage).
#[derive(Resource)]
struct MaskMaterial(Handle<StandardMaterial>);

/// Per-view outline settings, uploaded to the composite shader. Present on the main
/// camera only while something is outlined (its presence also gates the pass).
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
struct OutlineSettings {
    color: Vec4,
    /// x = ring radius in pixels; y/z/w unused (a full `vec4` keeps the uniform a
    /// clean 16-byte multiple on WebGL2).
    params: Vec4,
}

fn mask_texture() -> Image {
    // Sized to a placeholder; `resize_mask_image` matches it to the window each
    // frame. Use the *sRGB* format: a plain `Rgba8Unorm` render target makes Bevy
    // request an extra sRGB `view_formats` on its ViewTarget, which WebGL2 does not
    // support (blank screen). We only read the alpha (coverage), so sRGB is moot.
    Image::new_target_texture(16, 16, TextureFormat::Rgba8UnormSrgb, None)
}

fn setup_mask_resources(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(OutlineMaskImage(images.add(mask_texture())));
    commands.insert_resource(MaskMaterial(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        ..default()
    })));
}

/// Spawn the mask camera only *after* egui has claimed the main camera as its
/// primary context. `bevy_egui` attaches the primary context to the first camera
/// that appears; if the mask camera were spawned first it would steal egui, which
/// would then render the whole UI INTO the mask and get it outlined. Gating on the
/// main camera already carrying `PrimaryEguiContext` guarantees egui's
/// once-only guard has fired, so the mask camera never gets a context.
fn spawn_mask_camera(
    mut commands: Commands,
    mask: Res<OutlineMaskImage>,
    existing: Query<(), With<MaskCam>>,
    main_ready: Query<(), (With<Camera3d>, With<PrimaryEguiContext>, Without<MaskCam>)>,
) {
    if !existing.is_empty() || main_ready.is_empty() {
        return;
    }
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -1,
            // Transparent black: uncovered texels read alpha 0.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        // No tonemapping on a coverage mask (and the default TonyMcMapface LUT
        // isn't built into this binary).
        Tonemapping::None,
        Msaa::Off,
        RenderTarget::Image(mask.0.clone().into()),
        RenderLayers::layer(MASK_LAYER),
        MaskCam,
    ));
}

/// Keep the mask camera's world pose + lens locked to the main camera so the mask
/// lines up with the scene. Runs AFTER `TransformSystems::Propagate` and writes the
/// mask camera's `GlobalTransform` directly (it's a root entity, so that's its render
/// pose): copying the main camera's *just-propagated* `GlobalTransform` this way
/// avoids the one-frame lag that offsets the outline while you move. `Transform` is
/// updated too so the next frame's propagation starts consistent.
fn sync_mask_camera(
    main: Query<(&GlobalTransform, &Projection), (With<Camera3d>, Without<MaskCam>)>,
    mut mask: Query<(&mut Transform, &mut GlobalTransform, &mut Projection), With<MaskCam>>,
) {
    let Some((main_gt, main_proj)) = main.iter().next() else {
        return;
    };
    let Ok((mut t, mut gt, mut p)) = mask.single_mut() else {
        return;
    };
    *t = main_gt.compute_transform();
    *gt = *main_gt;
    *p = main_proj.clone();
}

/// Match the mask render target to the window's physical resolution so the
/// dilation radius is measured in real screen pixels.
fn resize_mask_image(
    mask: Res<OutlineMaskImage>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Ok(window) = window.single() else {
        return;
    };
    let (w, h) = (
        window.physical_width().max(1),
        window.physical_height().max(1),
    );
    if let Some(mut image) = images.get_mut(&mask.0) {
        if image.width() != w || image.height() != h {
            image.resize(Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            });
        }
    }
}

/// Add/remove [`OutlineSettings`] on the main camera so the full-screen composite
/// pass only runs while at least one part is outlined.
fn toggle_outline_pass(
    mut commands: Commands,
    outlined: Query<(), With<Outlined>>,
    cam: Query<(Entity, Has<OutlineSettings>), (With<Camera3d>, Without<MaskCam>)>,
) {
    let want = !outlined.is_empty();
    for (entity, has) in cam.iter() {
        if want && !has {
            commands.entity(entity).insert(OutlineSettings {
                color: OUTLINE_COLOR,
                params: Vec4::new(OUTLINE_WIDTH_PX, 0.0, 0.0, 0.0),
            });
        } else if !want && has {
            commands.entity(entity).remove::<OutlineSettings>();
        }
    }
}

/// Outline the part the local player has focused (grab preview) or is holding.
/// Toggles the [`Outlined`] marker only on change (tracked by `lit`).
fn sync_outline_focus(
    mut commands: Commands,
    player: Query<&FocusedInteractable, With<Player>>,
    mut lit: Local<Option<Entity>>,
) {
    let focused = player.iter().next().and_then(|f| f.0);
    if *lit == focused {
        return;
    }
    if let Some(prev) = *lit {
        if let Ok(mut entity) = commands.get_entity(prev) {
            entity.remove::<Outlined>();
        }
    }
    if let Some(now) = focused {
        commands.entity(now).insert(Outlined);
    }
    *lit = focused;
}

/// Give each newly-[`Outlined`] part its mask proxies: a copy of every mesh in its
/// subtree (skipping flame plumes) on the mask render layer, so the mask camera
/// captures the whole silhouette — a rocket's flare together with its body.
fn spawn_outlines(
    mut commands: Commands,
    mask_material: Res<MaskMaterial>,
    newly: Query<Entity, (With<Outlined>, Without<OutlineChild>)>,
    // Mesh-bearing entities that are NOT flame plumes (flames aren't part of the
    // silhouette we outline).
    meshes: Query<&Mesh3d, Without<MeshMaterial3d<FlameMaterial>>>,
    children: Query<&Children>,
) {
    for entity in newly.iter() {
        let mut proxies = Vec::new();
        let mut stack = vec![entity];
        while let Some(e) = stack.pop() {
            if let Ok(mesh) = meshes.get(e) {
                let proxy = commands
                    .spawn((
                        Mesh3d(mesh.0.clone()),
                        MeshMaterial3d(mask_material.0.clone()),
                        Transform::default(),
                        RenderLayers::layer(MASK_LAYER),
                        NotShadowCaster,
                    ))
                    .id();
                commands.entity(e).add_child(proxy);
                proxies.push(proxy);
            }
            if let Ok(child_list) = children.get(e) {
                stack.extend(child_list.iter());
            }
        }
        commands.entity(entity).insert(OutlineChild(proxies));
    }
}

/// Drop the mask proxies once a part loses [`Outlined`]. (A part despawned outright
/// takes its child proxies with it.)
fn despawn_outlines(
    mut commands: Commands,
    stale: Query<(Entity, &OutlineChild), Without<Outlined>>,
) {
    for (entity, child) in stale.iter() {
        for &proxy in &child.0 {
            if let Ok(mut proxy) = commands.get_entity(proxy) {
                proxy.despawn();
            }
        }
        commands.entity(entity).remove::<OutlineChild>();
    }
}

// ---------------------------------------------------------------------------
// Render world: the composite pass
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct OutlinePipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

fn init_outline_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    fullscreen_shader: Res<FullscreenShader>,
    pipeline_cache: Res<PipelineCache>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "outline_bind_group_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }), // mask
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<OutlineSettings>(true),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor::default());
    let pipeline_id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("outline_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader: OUTLINE_SHADER_HANDLE,
            targets: vec![Some(ColorTargetState {
                // Matches the non-HDR main view texture; alpha-blend the ring over it.
                format: TextureFormat::Rgba8UnormSrgb,
                blend: Some(BlendState::ALPHA_BLENDING),
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    });
    commands.insert_resource(OutlinePipeline {
        layout,
        sampler,
        pipeline_id,
    });
}

fn outline_pass(
    view: ViewQuery<(&ViewTarget, &DynamicUniformIndex<OutlineSettings>)>,
    pipeline: Option<Res<OutlinePipeline>>,
    pipeline_cache: Res<PipelineCache>,
    settings: Res<ComponentUniforms<OutlineSettings>>,
    mask: Res<OutlineMaskImage>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    let (Some(render_pipeline), Some(mask_gpu)) = (
        pipeline_cache.get_render_pipeline(pipeline.pipeline_id),
        gpu_images.get(&mask.0),
    ) else {
        return;
    };
    let Some(settings_binding) = settings.uniforms().binding() else {
        return;
    };
    let (view_target, settings_index) = view.into_inner();

    // Composite the ring straight onto the main texture with alpha blending. We
    // never read the scene texture, so there's no need for `post_process_write`'s
    // ping-pong buffers (which require `VIEW_FORMATS`, unsupported on WebGL2).
    let bind_group = ctx.render_device().create_bind_group(
        "outline_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline.layout),
        &BindGroupEntries::sequential((
            &mask_gpu.texture_view,
            &pipeline.sampler,
            settings_binding.clone(),
        )),
    );

    let mut pass = ctx.command_encoder().begin_render_pass(&RenderPassDescriptor {
        label: Some("outline_pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: view_target.main_texture_view(),
            depth_slice: None,
            resolve_target: None,
            // Load (preserve the scene) then blend the ring on top.
            ops: Operations {
                load: LoadOp::Load,
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(render_pipeline);
    pass.set_bind_group(0, &bind_group, &[settings_index.index()]);
    pass.draw(0..3, 0..1);
}
