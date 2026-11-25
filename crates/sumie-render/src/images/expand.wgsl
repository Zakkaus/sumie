// Expands tightly packed RGB, gray, or gray-alpha bytes into an RGBA8 texture.
struct Source {
    width: u32,
    channels: u32,
    // First 32-bit word of this image in the scratch texture.
    base: u32,
    // Words per scratch row.
    stride: u32,
}

@group(0) @binding(0) var words: texture_2d<u32>;
@group(0) @binding(1) var<uniform> source: Source;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn byte(index: u32) -> u32 {
    let word = source.base + index / 4u;
    let texel = textureLoad(words, vec2<u32>(word % source.stride, word / source.stride), 0).x;
    return (texel >> (index % 4u * 8u)) & 0xffu;
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(position.xy);
    let start = (pixel.y * source.width + pixel.x) * source.channels;
    var rgba: vec4<u32>;
    switch source.channels {
        case 1u: {
            rgba = vec4<u32>(vec3<u32>(byte(start)), 255u);
        }
        case 2u: {
            rgba = vec4<u32>(vec3<u32>(byte(start)), byte(start + 1u));
        }
        default: {
            rgba = vec4<u32>(byte(start), byte(start + 1u), byte(start + 2u), 255u);
        }
    }
    // The unorm target stores these bytes unchanged.
    return vec4<f32>(rgba) / 255.0;
}
