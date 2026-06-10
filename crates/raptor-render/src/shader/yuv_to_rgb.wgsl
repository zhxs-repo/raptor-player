// YUV420P/NV12 → RGB 转换 shader
// 使用 BT.601 系数矩阵

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // 全屏三角形 strip（4 个顶点）
    var pos = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var uv = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VertexOutput;
    out.position = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    out.uv = uv[vertex_index];
    return out;
}

@group(0) @binding(0) var y_texture: texture_2d<f32>;
@group(0) @binding(1) var uv_texture: texture_2d<f32>;
@group(0) @binding(2) var yuv_sampler: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y = textureSample(y_texture, yuv_sampler, in.uv).r;
    let uv = textureSample(uv_texture, yuv_sampler, in.uv).rg;

    // BT.601 YUV → RGB
    let yy = y - 0.0627451;  // 16/255
    let u = uv.r - 0.5;
    let v = uv.g - 0.5;

    var r = yy * 1.16438 + v * 1.59603;
    var g = yy * 1.16438 - u * 0.39176 - v * 0.81297;
    var b = yy * 1.16438 + u * 2.01723;

    r = clamp(r, 0.0, 1.0);
    g = clamp(g, 0.0, 1.0);
    b = clamp(b, 0.0, 1.0);

    return vec4<f32>(r, g, b, 1.0);
}
