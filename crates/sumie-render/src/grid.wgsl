struct Frame {
    size: vec4<u32>,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var coverage_atlas: texture_2d<f32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) atlas_position: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) luminances: vec2<f32>,
    @location(4) @interpolate(flat) srgb_luminances: vec2<f32>,
    @location(3) @interpolate(flat) glyph: u32,
}

const LUMINANCE = vec3<f32>(0.2126, 0.7152, 0.0722);
const MIN_CONTRAST = 1.1;

fn linear_rgb(srgb: vec3<f32>) -> vec3<f32> {
    return select(pow((srgb + 0.055) / 1.055, vec3<f32>(2.4)), srgb / 12.92, srgb <= vec3<f32>(0.04045));
}

fn linear_luma(srgb: f32) -> f32 {
    return select(pow((srgb + 0.055) / 1.055, 2.4), srgb / 12.92, srgb <= 0.04045);
}

fn srgb_luma(linear: f32) -> f32 {
    return select(1.055 * pow(linear, 1.0 / 2.4) - 0.055, 12.92 * linear, linear <= 0.0031308);
}

fn contrasted(fg: vec3<f32>, bg_luma: f32) -> vec3<f32> {
    let fg_luma = dot(fg, LUMINANCE);
    let light = max(fg_luma, bg_luma) + 0.05;
    let dark = min(fg_luma, bg_luma) + 0.05;
    if light >= MIN_CONTRAST * dark {
        return fg;
    }
    let background = bg_luma + 0.05;
    if 1.05 / background > background / 0.05 {
        let desired = background * MIN_CONTRAST - 0.05;
        return mix(fg, vec3<f32>(1.0), (desired - fg_luma) / (1.0 - fg_luma));
    }
    let desired = background / MIN_CONTRAST - 0.05;
    return fg * (desired / fg_luma);
}

@vertex
fn vertex(
    @builtin(vertex_index) index: u32,
    @location(0) origin: vec2<i32>,
    @location(1) size: vec2<u32>,
    @location(2) atlas_origin: vec2<u32>,
    @location(3) color: vec4<f32>,
    @location(4) background: vec4<f32>,
) -> VertexOut {
    let corner = vec2<u32>(index & 1u, index >> 1u);
    let offset = size * corner;
    let pixel = vec2<f32>(origin + vec2<i32>(offset));
    let clip = pixel * 2.0 / vec2<f32>(frame.size.xy) - 1.0;
    var out: VertexOut;
    out.position = vec4<f32>(clip.x, -clip.y, 0.0, 1.0);
    out.glyph = u32(atlas_origin.x != 0xffffffffu);
    var fg = linear_rgb(color.rgb);
    if out.glyph != 0u {
        let bg_luma = dot(linear_rgb(background.rgb) * background.a, LUMINANCE);
        fg = contrasted(fg, bg_luma);
        out.luminances = vec2<f32>(dot(fg, LUMINANCE), bg_luma);
        out.srgb_luminances = vec2<f32>(srgb_luma(out.luminances.x), srgb_luma(bg_luma));
        out.atlas_position = vec2<f32>(atlas_origin + offset);
    }
    out.color = vec4<f32>(fg * color.a, color.a);
    return out;
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    if in.glyph == 0u {
        return in.color;
    }
    var alpha = textureLoad(coverage_atlas, vec2<i32>(in.atlas_position), 0).r;
    let fg_luma = in.luminances.x;
    let bg_luma = in.luminances.y;
    let delta = fg_luma - bg_luma;
    // Ghostty's luminance correction keeps gamma-space weight with linear RGB blending.
    if alpha > 0.0 && alpha < 1.0 && abs(delta) > 0.001 {
        let desired = linear_luma(mix(in.srgb_luminances.y, in.srgb_luminances.x, alpha));
        alpha = clamp((desired - bg_luma) / delta, 0.0, 1.0);
    }
    return in.color * alpha;
}
