//! YUV→RGB blit 管线与 UV 平面交织 — WgpuRenderer / ExternalRenderer 共享实现
//!
//! 两个渲染器只有 surface 来源不同（自管窗口 vs 宿主注入），
//! 视频上传/绘制的 GPU 资源构造完全一致，统一收敛到这里避免双份维护。

/// 构建 YUV 渲染所需的管线、绑定组与纹理。
///
/// `label_prefix` 用于区分调试标签来源（如 "ext"），空串则不加前缀。
pub(crate) fn setup_yuv_pipeline(
    device: &wgpu::Device,
    surface_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    label_prefix: &str,
) -> (
    wgpu::RenderPipeline,
    wgpu::BindGroup,
    wgpu::Texture,
    wgpu::Texture,
) {
    let lbl = |name: &str| -> String {
        if label_prefix.is_empty() {
            name.to_string()
        } else {
            format!("{label_prefix}_{name}")
        }
    };

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&lbl("yuv_to_rgb")),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader/yuv_to_rgb.wgsl").into()),
    });

    let y_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&lbl("y_texture")),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let uv_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&lbl("uv_texture")),
        size: wgpu::Extent3d {
            width: (width / 2).max(1),
            height: (height / 2).max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(&lbl("yuv_sampler")),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(&lbl("yuv_bind_group_layout")),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&lbl("yuv_bind_group")),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&y_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&uv_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&lbl("yuv_pipeline_layout")),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&lbl("yuv_render_pipeline")),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (render_pipeline, bind_group, y_texture, uv_texture)
}

/// 把平面 U/V 数据按 stride 交织成 RG8 纹理所需的行主序字节
pub(crate) fn interleave_uv_planes(
    u_data: &[u8],
    u_stride: usize,
    v_data: &[u8],
    v_stride: usize,
    uv_width: usize,
    uv_height: usize,
) -> Vec<u8> {
    let mut result = Vec::with_capacity(uv_width * uv_height * 2);
    for row in 0..uv_height {
        for col in 0..uv_width {
            let u = if row * u_stride + col < u_data.len() {
                u_data[row * u_stride + col]
            } else {
                128
            };
            let v = if row * v_stride + col < v_data.len() {
                v_data[row * v_stride + col]
            } else {
                128
            };
            result.push(u);
            result.push(v);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interleave_uv_planes() {
        let u = vec![10u8, 20, 30, 40];
        let v = vec![50u8, 60, 70, 80];
        let result = interleave_uv_planes(&u, 2, &v, 2, 2, 2);
        assert_eq!(result, vec![10, 50, 20, 60, 30, 70, 40, 80]);
    }

    #[test]
    fn test_interleave_uv_empty() {
        let result = interleave_uv_planes(&[], 0, &[], 0, 0, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_interleave_uv_out_of_range_fills_neutral() {
        // 数据短于 stride 覆盖范围时应以 128（中性色度）填充
        let u = vec![10u8];
        let v = vec![20u8];
        let result = interleave_uv_planes(&u, 2, &v, 2, 2, 1);
        assert_eq!(result, vec![10, 20, 128, 128]);
    }
}
