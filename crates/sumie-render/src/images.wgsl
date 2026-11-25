@group(0) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(0) var<uniform> exclusions: array<vec4<f32>, 2>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) source: vec2<f32>,
    @location(1) @interpolate(flat) crop: vec4<i32>,
    @location(2) @interpolate(flat) edges: vec4<f32>,
    @location(3) @interpolate(flat) border: vec4<f32>,
    @location(4) @interpolate(flat) ink: vec4<f32>,
    @location(5) @interpolate(flat) chip_size: f32,
}

fn linear_premultiplied(color: vec4<f32>) -> vec4<f32> {
    let rgb = select(pow((color.rgb + 0.055) / 1.055, vec3<f32>(2.4)), color.rgb / 12.92,
                     color.rgb <= vec3<f32>(0.04045));
    return vec4<f32>(rgb * color.a, color.a);
}

@vertex
fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) dest: vec4<f32>,
    @location(1) source: vec4<f32>,
    @location(2) crop: vec4<i32>,
    @location(3) edges: vec4<f32>,
    @location(4) border: vec4<f32>,
    @location(5) ink: vec4<f32>,
    @location(6) chip_size: u32,
) -> VertexOut {
    let corner = vec2<bool>((index & 1u) != 0u, (index & 2u) != 0u);
    var out: VertexOut;
    out.position = vec4<f32>(select(dest.xy, dest.zw, corner), 0.0, 1.0);
    out.source = select(source.xy, source.zw, corner);
    out.crop = crop;
    out.edges = edges;
    if border.a != 0.0 {
        out.border = linear_premultiplied(border);
    }
    if chip_size != 0u {
        out.ink = linear_premultiplied(ink);
    }
    out.chip_size = f32(chip_size);
    return out;
}

fn covered(pixel: vec2<f32>, rect: vec4<f32>) -> bool {
    return all(pixel >= rect.xy) && all(pixel < rect.zw);
}

fn premultiplied_texel(pos: vec2<i32>) -> vec4<f32> {
    // The texture holds sRGB bytes; decode RGB to linear before premultiplication.
    return linear_premultiplied(textureLoad(image, pos, 0));
}

fn filtered_row(pos: vec2<i32>, weight: f32) -> vec4<f32> {
    let first = premultiplied_texel(pos);
    if weight == 0.0 {
        return first;
    }
    return mix(first, premultiplied_texel(pos + vec2<i32>(1, 0)), weight);
}

fn sample_image(in: VertexOut) -> vec4<f32> {
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

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = b - a;
    return distance(p, a + d * clamp(dot(p - a, d) / dot(d, d), 0.0, 1.0));
}

// Diagonal expand arrows on a 16-unit icon canvas. Mirror the NE half through the center.
fn arrow_coverage(local: vec2<f32>, side: f32) -> f32 {
    let at = local * (16.0 / side) - vec2<f32>(8.0);
    let p = select(-at, at, at.x >= at.y);
    let tip = vec2<f32>(4.5, -4.5);
    let shaft = segment_distance(p, vec2<f32>(0.0), tip);
    let head = min(segment_distance(p, vec2<f32>(0.0, tip.y), tip),
                   segment_distance(p, vec2<f32>(tip.x, 0.0), tip));
    return clamp(0.5 + (0.85 - min(shaft, head)) * (side / 16.0), 0.0, 1.0);
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let pixel = in.position.xy;
    if covered(pixel, exclusions[0]) || covered(pixel, exclusions[1]) {
        discard;
    }
    if in.border.a != 0.0 {
        let to_end = in.edges.zw - pixel;
        if any(min(pixel - in.edges.xy, to_end) < vec2<f32>(1.0)) {
            if in.border.a == 1.0 {
                return in.border;
            }
            return in.border + sample_image(in) * (1.0 - in.border.a);
        }
        // The chip meets the frame at the true bottom-right, even when the pane clips it.
        if in.chip_size > 0.0 && all(to_end <= vec2<f32>(in.chip_size + 1.0)) {
            let local = vec2<f32>(in.chip_size + 1.0) - to_end;
            return mix(in.border, in.ink, arrow_coverage(local, in.chip_size));
        }
    }
    return sample_image(in);
}
