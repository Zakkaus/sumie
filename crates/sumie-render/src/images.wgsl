@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(1) @binding(0) var<uniform> exclusions: array<vec4<f32>, 2>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) dest: vec4<f32>,
    @location(1) uv: vec4<f32>,
) -> VertexOut {
    let corner = vec2<bool>((index & 1u) != 0u, (index & 2u) != 0u);
    var out: VertexOut;
    out.position = vec4<f32>(select(dest.xy, dest.zw, corner), 0.0, 1.0);
    out.uv = select(uv.xy, uv.zw, corner);
    return out;
}

fn covered(pixel: vec2<f32>, rect: vec4<f32>) -> bool {
    return all(pixel >= rect.xy) && all(pixel < rect.zw);
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    if covered(in.position.xy, exclusions[0]) || covered(in.position.xy, exclusions[1]) {
        discard;
    }
    let color = textureSampleLevel(image, image_sampler, in.uv, 0.0);
    return vec4<f32>(color.rgb * color.a, color.a);
}
