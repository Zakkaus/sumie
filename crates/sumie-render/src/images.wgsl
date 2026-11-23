@group(0) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(0) var<uniform> exclusions: array<vec4<f32>, 2>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) source: vec2<f32>,
    @location(1) @interpolate(flat) crop: vec4<i32>,
}

@vertex
fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) dest: vec4<f32>,
    @location(1) source: vec4<f32>,
    @location(2) crop: vec4<i32>,
) -> VertexOut {
    let corner = vec2<bool>((index & 1u) != 0u, (index & 2u) != 0u);
    var out: VertexOut;
    out.position = vec4<f32>(select(dest.xy, dest.zw, corner), 0.0, 1.0);
    out.source = select(source.xy, source.zw, corner);
    out.crop = crop;
    return out;
}

fn covered(pixel: vec2<f32>, rect: vec4<f32>) -> bool {
    return all(pixel >= rect.xy) && all(pixel < rect.zw);
}

fn premultiplied_texel(pos: vec2<i32>) -> vec4<f32> {
    // The sRGB texture load decodes RGB to linear before premultiplication.
    let color = textureLoad(image, pos, 0);
    return vec4<f32>(color.rgb * color.a, color.a);
}

fn filtered_row(pos: vec2<i32>, weight: f32) -> vec4<f32> {
    let first = premultiplied_texel(pos);
    if weight == 0.0 {
        return first;
    }
    return mix(first, premultiplied_texel(pos + vec2<i32>(1, 0)), weight);
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    if covered(in.position.xy, exclusions[0]) || covered(in.position.xy, exclusions[1]) {
        discard;
    }
    let lo = vec2<f32>(in.crop.xy);
    let hi = vec2<f32>(abs(in.crop.zw) - vec2<i32>(1));
    let center = in.source - vec2<f32>(0.5);
    // Raster interpolation can miss exact texel centers even at native size.
    let snapped = select(center, round(center), in.crop.zw < vec2<i32>(0));
    let pos = clamp(snapped, lo, hi);
    let base = vec2<i32>(pos);
    let weight = pos - vec2<f32>(base);
    let first = filtered_row(base, weight.x);
    if weight.y == 0.0 {
        return first;
    }
    return mix(first, filtered_row(base + vec2<i32>(0, 1), weight.x), weight.y);
}
