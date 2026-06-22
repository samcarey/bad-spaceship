// Bevy 0.12 stores `Mesh.model` as a compressed affine `mat3x4`, so it can no
// longer be multiplied directly like the old `mat4x4`. Use the `mesh_functions`
// helpers, which unpack the affine matrix and also handle the WebGL2 batching
// path (where `mesh` is a fixed-size uniform array, not a storage buffer).
#import bevy_pbr::mesh_functions::{get_model_matrix, mesh_position_local_to_clip}

struct GizmoMaterial {
    color: vec4<f32>,
};

// Bevy 0.13 moved custom material bind groups from @group(1) to @group(2)
// (group 1 is now the view-independent mesh binding).
@group(2) @binding(0)
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
        get_model_matrix(vertex.instance_index),
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
