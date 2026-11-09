// Fullscreen background shader for the "GPU blur" and "flow" background
// modes. It samples a small (160 px) cover thumbnail, applies a separable
// gaussian approximation on the GPU, and optionally animates the result.
//
// The pipeline is created lazily inside egui-wgpu's callback resources, so it
// shares the frame's device/queue and render pass with the rest of the UI.

struct Uniforms {
    // Top-left of the painted rect in framebuffer pixels.
    origin: vec2<f32>,
    size: vec2<f32>,
    time: f32,
    mode: u32,
    // Blur radius in screen pixels.
    blur: f32,
    // How strongly the theme tint is mixed over the cover.
    opacity: f32,
    // 1 when the target format is not sRGB and the shader must encode.
    manual_srgb: u32,
    _pad0: u32,
    _pad1: vec2<u32>,
    tint: vec4<f32>,
    accent: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_smp: sampler;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle: one triangle covers the whole viewport without a
    // vertex buffer.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

// 17-tap gaussian-ish disc. The source texture is only 160 px wide, so the
// bilinear sampler already removes most high frequencies; these taps turn the
// thumbnail into a smooth, cover-coloured wash.
fn blurred(uv: vec2<f32>, radius: f32) -> vec3<f32> {
    let r = max(radius, 2.0) / max(u.size.y, 1.0);
    var color = textureSample(src_tex, src_smp, uv).rgb * 0.20;
    let step = 6.2831853 / 8.0;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        let angle = f32(i) * step;
        let offset = vec2<f32>(cos(angle), sin(angle)) * r;
        color += textureSample(src_tex, src_smp, uv + offset).rgb * 0.06;
        color += textureSample(src_tex, src_smp, uv + offset * 1.9).rgb * 0.04;
    }
    return color;
}

fn rotate(v: vec2<f32>, angle: f32) -> vec2<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec2<f32>(v.x * c - v.y * s, v.x * s + v.y * c);
}

fn flow_color(uv: vec2<f32>) -> vec3<f32> {
    let t = u.time * 0.05;
    let centered = uv - vec2<f32>(0.5, 0.5);
    let zoom = 1.0 + 0.07 * sin(t * 0.7);
    let drift = vec2<f32>(0.06 * sin(t), 0.05 * cos(t * 0.8));
    let sample_uv = rotate(centered / zoom, 0.10 * sin(t * 0.5)) + vec2<f32>(0.5, 0.5) + drift;
    let base = blurred(sample_uv, max(u.blur, 10.0));

    // Slow accent blobs give the gradient movement without a second pass.
    let aspect = u.size.x / max(u.size.y, 1.0);
    let p = vec2<f32>(uv.x * aspect, uv.y);
    let b1 = exp(-9.0 * length(p - vec2<f32>(0.24 + 0.16 * sin(t * 1.1), 0.30 + 0.12 * cos(t * 0.9))));
    let b2 = exp(-8.0 * length(p - vec2<f32>(0.80 + 0.10 * cos(t * 0.8), 0.70 + 0.14 * sin(t * 1.3))));
    let b3 = exp(-10.0 * length(p - vec2<f32>(0.52 + 0.18 * cos(t * 1.6), 0.12 + 0.10 * sin(t))));
    let glow = clamp(b1 + b2 + b3, 0.0, 1.4);
    return base + u.accent.rgb * glow * 0.12;
}

fn to_srgb(color: vec3<f32>) -> vec3<f32> {
    let lo = color * 12.92;
    let hi = 1.055 * pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, color <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = (frag.xy - u.origin) / max(u.size, vec2<f32>(1.0));
    // Flow mode draws its own blurred sample; computing the static blur first
    // would double the 17 texture taps for every pixel.
    var color: vec3<f32>;
    if u.mode == 1u {
        color = flow_color(uv);
    } else {
        color = blurred(uv, u.blur);
    }

    // Keep text readable: mix the theme tint over the cover, then darken the
    // lower third slightly (the player bar and lyrics sit there).
    color = mix(color, u.tint.rgb, clamp(u.opacity, 0.0, 0.95));
    let vignette = 1.0 - smoothstep(0.30, 0.95, length(uv - vec2<f32>(0.5, 0.5)));
    color = color * (0.86 + 0.20 * vignette);
    color = mix(color, u.tint.rgb, smoothstep(0.35, 1.0, uv.y) * 0.28);

    if u.manual_srgb == 1u {
        color = to_srgb(color);
    }
    return vec4<f32>(color, 1.0);
}
