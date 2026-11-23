struct Frame {
    size: vec2<u32>,
    minimum_contrast: f32,
    padding: u32,
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
const CURSOR_GLYPH: u32 = 0x80000000u;

fn linear_rgb(srgb: vec3<f32>) -> vec3<f32> {
    return select(pow((srgb + 0.055) / 1.055, vec3<f32>(2.4)), srgb / 12.92, srgb <= vec3<f32>(0.04045));
}

fn linear_luma(srgb: f32) -> f32 {
    return select(pow((srgb + 0.055) / 1.055, 2.4), srgb / 12.92, srgb <= 0.04045);
}

fn srgb_luma(linear: f32) -> f32 {
    return select(1.055 * pow(linear, 1.0 / 2.4) - 0.055, 12.92 * linear, linear <= 0.0031308);
}

fn contrasted_luma(fg_luma: f32, bg_luma: f32, minimum_contrast: f32) -> f32 {
    let light = max(fg_luma, bg_luma) + 0.05;
    let dark = min(fg_luma, bg_luma) + 0.05;
    if light >= minimum_contrast * dark {
        return fg_luma;
    }
    // Ghostty's contrasted_color picks the higher-contrast endpoint. Solve for the
    // requested luminance instead of using that endpoint unless the ratio is unreachable.
    // https://github.com/ghostty-org/ghostty/blob/c42ed707588c2de57367c162ef378f651c5716fb/src/renderer/shaders/glsl/common.glsl#L73-L109
    let background = bg_luma + 0.05;
    if 1.05 * 0.05 > background * background {
        return min(background * minimum_contrast - 0.05, 1.0);
    }
    return max(background / minimum_contrast - 0.05, 0.0);
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
    let clip = pixel * 2.0 / vec2<f32>(frame.size) - 1.0;
    var out: VertexOut;
    out.position = vec4<f32>(clip.x, -clip.y, 0.0, 1.0);
    out.glyph = u32(atlas_origin.x != 0xffffffffu);
    var fg = linear_rgb(color.rgb);
    if out.glyph != 0u {
        let bg_luma = dot(linear_rgb(background.rgb) * background.a, LUMINANCE);
        let fg_luma = dot(fg, LUMINANCE);
        var adjusted_luma = fg_luma;
        if frame.minimum_contrast > 1.0 && (atlas_origin.x & CURSOR_GLYPH) == 0u {
            adjusted_luma = contrasted_luma(fg_luma, bg_luma, frame.minimum_contrast);
            if adjusted_luma > fg_luma {
                fg = mix(fg, vec3<f32>(1.0), (adjusted_luma - fg_luma) / (1.0 - fg_luma));
            } else if adjusted_luma < fg_luma {
                fg *= adjusted_luma / fg_luma;
            }
        }
        out.luminances = vec2<f32>(adjusted_luma, bg_luma);
        out.srgb_luminances = vec2<f32>(srgb_luma(out.luminances.x), srgb_luma(bg_luma));
        out.atlas_position = vec2<f32>(vec2<u32>(atlas_origin.x & ~CURSOR_GLYPH, atlas_origin.y) + offset);
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
    // Mild stem darkening: lifts partial edge pixels, leaves empty and full ones.
    alpha += 0.24 * alpha * (1.0 - alpha);
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
