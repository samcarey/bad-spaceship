// Bevy 0.12 stores `Mesh.model` as a compressed affine `mat3x4`, so it can no
// longer be multiplied directly like the old `mat4x4`. Use the `mesh_functions`
// helpers, which unpack the affine matrix and also handle the WebGL2 batching
// path (where `mesh` is a fixed-size uniform array, not a storage buffer).
// Bevy 0.14 renamed `get_model_matrix` → `get_world_from_local` as part of its
// `<dest>_from_<src>` matrix-naming convention.
#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_clip}

struct GizmoMaterial {
    color: vec4<f32>,
};

// Bevy 0.13 moved custom material bind groups from @group(1) to @group(2).
// Bevy 0.17's wgpu 25 shuffled the 3D bind groups again (mesh resources took
// group 2, materials moved to group 3); the `#{MATERIAL_BIND_GROUP}` shader def
// expands to the current material group so this stays correct across the change.
@group(#{MATERIAL_BIND_GROUP}) @binding(0)
var<uniform> material: GizmoMaterial;

struct Vertex {
    // Needed to look up this instance's model matrix.
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    var modified_clip = mesh_position_local_to_clip(
        get_world_from_local(vertex.instance_index),
        vec4<f32>(vertex.position, 1.0),
    );
    // Remap the depth to be right in front of the camera. We remap (mix) here instead of hardcoding
    // the depth, to ensure the components of the gizmo mesh are sorted correctly.
    modified_clip.z = mix(0.999, 1.0, modified_clip.z);
    out.clip_position = modified_clip;
    out.uv = vertex.uv;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return material.color;
}
