@group(0) @binding(0) var scene: texture_2d<f32>;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn srgb(linear: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055,
                  12.92 * linear, linear <= vec3<f32>(0.0031308));
}

fn linear(encoded: vec3<f32>) -> vec3<f32> {
    return select(pow((encoded + 0.055) / 1.055, vec3<f32>(2.4)),
                  encoded / 12.92, encoded <= vec3<f32>(0.04045));
}

@fragment
fn straight(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    let color = textureLoad(scene, vec2<i32>(pixel.xy), 0);
    if color.a == 0.0 || color.a == 1.0 {
        return color;
    }
    return vec4<f32>(clamp(color.rgb / color.a, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
}

@fragment
fn premultiplied(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    let color = textureLoad(scene, vec2<i32>(pixel.xy), 0);
    if color.a == 0.0 {
        return vec4<f32>(0.0);
    }
    if color.a == 1.0 {
        return color;
    }
    // The compositor expects its stored sRGB channels multiplied by alpha. The render attachment
    // will encode that value on write; linear-light premultiplied RGB would be encoded too bright.
    let rgb = clamp(color.rgb / color.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(linear(srgb(rgb) * color.a), color.a);
}
