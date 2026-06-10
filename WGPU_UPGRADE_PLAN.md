# wgpu 升级到 v29.0.3 方案

## 📊 当前状态

| 项目 | 当前版本 | 目标版本 | 差距 |
|------|----------|----------|------|
| **wgpu** | `0.20` | `29.0.3` | **9 个大版本** |
| **发布日期** | 2024-06 | 2026-05-02 | 约 2 年 |
| **MSRV** | 1.70+ | **1.87** | 需检查兼容性 |

---

## 🎯 核心变更概览（v0.20 → v29.0.3）

### 1. **重大 API 破坏性变更**

#### 1.1 Surface 纹理获取（关键变更）
```rust
// ❌ 旧代码 (v0.20)
match surface.get_current_texture() {
    Ok(frame) => { /* render */ }
    Err(wgpu::SurfaceError::Lost) => { /* reconfigure */ }
}

// ✅ 新代码 (v29.0.3)
match surface.get_current_texture() {
    wgpu::CurrentSurfaceTexture::Success(frame) => { /* render */ }
    wgpu::CurrentSurfaceTexture::Timeout | 
    wgpu::CurrentSurfaceTexture::Occluded => { /* skip frame */ }
    wgpu::CurrentSurfaceTexture::Outdated | 
    wgpu::CurrentSurfaceTexture::Suboptimal(frame) => { /* reconfigure */ }
    wgpu::CurrentSurfaceTexture::Lost => { /* reconfigure or recreate device */ }
    wgpu::CurrentSurfaceTexture::Validation => { /* handle error */ }
}
```
**影响范围**: `wgpu_renderer.rs` 第 272-286 行  
**风险等级**: 🔴 高

---

#### 1.2 InstanceDescriptor 初始化（Display Handle 变更）
```rust
// ❌ 旧代码 (v0.20)
let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
    backends: wgpu::Backends::all(),
    ..Default::default()
});

// ✅ 新代码 (v29.0.3) - 需要 winit EventLoop 的 DisplayHandle
let instance = wgpu::Instance::new(
    wgpu::InstanceDescriptor::new_with_display_handle(
        Box::new(event_loop.owned_display_handle())
    )
);
```
**影响范围**: `wgpu_renderer.rs` 第 69-72 行  
**风险等级**: 🔴 高  
**注意**: 必须在创建 EventLoop 后才能获取 DisplayHandle

---

#### 1.3 PipelineLayoutDescriptor bind_group_layouts
```rust
// ❌ 旧代码 (v0.20)
let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
    label: Some("yuv_pipeline_layout"),
    bind_group_layouts: &[&bind_group_layout],
    push_constant_ranges: &[],
});

// ✅ 新代码 (v29.0.3) - 支持 Option 以允许 gaps
let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
    label: Some("yuv_pipeline_layout"),
    bind_group_layouts: &[Some(&bind_group_layout)], // 包装为 Some
    push_constant_ranges: &[],
});
```
**影响范围**: `wgpu_renderer.rs` 第 419-423 行  
**风险等级**: 🟡 中

---

#### 1.4 WriteOnly 类型（缓冲区写映射）
```rust
// ❌ 旧代码 (v0.20) - 直接返回 &mut [u8]
let mut buffer_slice = buffer.slice(..).get_mapped_range_mut();
buffer_slice.copy_from_slice(data);

// ✅ 新代码 (v29.0.3) - 返回 WriteOnly<[u8]>
let buffer_slice = buffer.slice(..).get_mapped_range_mut();
// WriteOnly 不支持 DerefMut，需要使用 slice/copy 方法
buffer_slice.copy_from_slice(data); // API 类似，但类型不同
```
**影响范围**: 当前代码未使用 buffer 映射，但未来扩展需注意  
**风险等级**: 🟢 低（当前无影响）

---

#### 1.5 DepthStencilState 字段变为 Optional
```rust
// ❌ 旧代码 (v0.20)
depth_stencil: Some(wgpu::DepthStencilState {
    format: wgpu::TextureFormat::Depth32Float,
    depth_write_enabled: true,
    depth_compare: wgpu::CompareFunction::Less,
    stencil: wgpu::StencilState::default(),
    bias: wgpu::DepthBiasState::default(),
}),

// ✅ 新代码 (v29.0.3)
depth_stencil: Some(wgpu::DepthStencilState {
    format: wgpu::TextureFormat::Depth32Float,
    depth_write_enabled: Some(true),      // 变为 Option<bool>
    depth_compare: Some(wgpu::CompareFunction::Less), // 变为 Option
    stencil: wgpu::StencilState::default(),
    bias: wgpu::DepthBiasState::default(),
}),
```
**影响范围**: 当前渲染器不使用深度模板，但未来扩展需注意  
**风险等级**: 🟢 低

---

### 2. **新增特性与优化**

#### 2.1 DX12 Agility SDK 支持（Windows 重要优化）
```rust
// 可配置使用最新 DX12 运行时，无需等待 Windows 更新
let options = wgpu::Dx12BackendOptions {
    agility_sdk: Some(wgpu::Dx12AgilitySDK {
        sdk_version: 619, // 匹配 D3D12Core.dll 版本
        sdk_path: "path/to/sdk/bin/x64".into(),
    }),
    ..Default::default()
};
```
**收益**: 
- 解锁最新 DX12 特性
- 修复 NVIDIA 多线程崩溃问题（v28.0.1 已修复）
- 更好的 GPU 兼容性

---

#### 2.2 Mesh Shaders（网格着色器）
- 完全支持 Vulkan
- Metal/DX12 支持 passthrough shaders
- 适合 meshlet 渲染、高级剔除技术
- **当前项目不需要**，但为未来 HDR/高级渲染预留能力

---

#### 2.3 性能改进
| 版本 | 改进内容 | 对项目的收益 |
|------|----------|-------------|
| v28.0.1 | 修复 NVIDIA 多线程 Present 崩溃 | 🔴 **关键修复**（避免崩溃） |
| v29.0.0 | 主后端正确报告 limits | 更好的兼容性 |
| v29.0.3 | 修复 late bindings 更新 | 减少渲染错误 |

---

#### 2.4 其他新特性
- `FLOAT32_BLENDABLE` 支持（Vulkan/Metal）
- Ray Tracing 加速结构（TLAS binding array）
- `@builtin(draw_index)` Vulkan 支持
- WebGPU 调试标记支持
- 改进的错误报告和验证

---

## 📋 升级实施计划

### 阶段 1：准备工作（0.5 天）

#### 1.1 更新 Cargo.toml
```toml
[workspace.dependencies]
wgpu = "29.0"  # 从 "0.20" 升级
winit = "0.30"  # 可能需要升级以配合 DisplayHandle
```

#### 1.2 MSRV 检查
- wgpu v29 要求 Rust **1.87**
- 检查项目其他依赖的 MSRV 兼容性
- 更新 `rust-toolchain.toml`（如有）

---

### 阶段 2：核心 API 迁移（2-3 天）

#### 2.1 Instance 创建（🔴 高优先级）
**文件**: `crates/raptor-render/src/wgpu_renderer.rs`

```rust
// 修改 EventLoop 创建逻辑
let mut builder = EventLoopBuilder::new();
#[cfg(target_os = "windows")]
builder.with_any_thread(true);
let event_loop = builder.build()...;

// ✅ 新增：获取 DisplayHandle
let display_handle = event_loop.owned_display_handle();

let instance = wgpu::Instance::new(
    wgpu::InstanceDescriptor::new_with_display_handle(
        Box::new(display_handle)
    )
);
```

---

#### 2.2 Surface 错误处理（🔴 高优先级）
**文件**: `crates/raptor-render/src/wgpu_renderer.rs` 第 272-286 行

```rust
// 替换原有的 Result 模式匹配
let surface_texture = match self.surface.get_current_texture() {
    wgpu::CurrentSurfaceTexture::Success(frame) => frame,
    wgpu::CurrentSurfaceTexture::Timeout |
    wgpu::CurrentSurfaceTexture::Occluded => {
        tracing::warn!("Skip frame: timeout/occluded");
        return;
    }
    wgpu::CurrentSurfaceTexture::Outdated |
    wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
        tracing::warn!("Reconfigure surface: outdated/suboptimal");
        self.surface.configure(&self.device, &self.surface_config);
        return;
    }
    wgpu::CurrentSurfaceTexture::Lost => {
        tracing::error!("Surface lost, need reconfigure or recreate device");
        self.window_should_close = true;
        return;
    }
    wgpu::CurrentSurfaceTexture::Validation => {
        tracing::error!("Validation error in surface acquisition");
        return;
    }
};
```

---

#### 2.3 Pipeline Layout（🟡 中优先级）
**文件**: `crates/raptor-render/src/wgpu_renderer.rs` 第 419-423 行

```rust
let pipeline_layout = device.create_pipeline_layout(
    &wgpu::PipelineLayoutDescriptor {
        label: Some("yuv_pipeline_layout"),
        bind_group_layouts: &[Some(&bind_group_layout)], // 添加 Some
        push_constant_ranges: &[],
    }
);
```

---

### 阶段 3：测试与验证（1-2 天）

#### 3.1 编译检查
```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
```

#### 3.2 功能测试矩阵
| 平台 | 测试项 | 预期结果 |
|------|--------|----------|
| **Windows** | 窗口创建、YUV 渲染、关闭 | ✅ 无崩溃 |
| **Windows** | NVIDIA GPU 多线程 Present | ✅ 无崩溃（v28.0.1 修复） |
| **macOS** | Metal 后端、Retina DPI | ✅ 正常渲染 |
| **Linux** | Vulkan/X11/Wayland | ✅ 正常渲染 |

#### 3.3 性能基准测试
- 对比升级前后 FPS
- 内存占用对比
- GPU 利用率监控

---

### 阶段 4：可选优化（1 天）

#### 4.1 DX12 Agility SDK 集成（仅 Windows）
```rust
#[cfg(target_os = "windows")]
let dx12_options = wgpu::Dx12BackendOptions {
    agility_sdk: Some(wgpu::Dx12AgilitySDK {
        sdk_version: 619,
        sdk_path: std::env::var("WGPU_DX12_AGILITY_SDK_PATH")
            .unwrap_or_else(|_| "C:\\DX12AgilitySDK\\bin\\x64".to_string())
            .into(),
    }),
    ..Default::default()
};

let instance = wgpu::Instance::new(
    wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        dx12_backend_options: dx12_options,
        ..Default::default()
    }
);
```

#### 4.2 错误处理增强
利用新版 wgpu 更详细的错误类型改进日志输出。

---

## ⚠️ 风险评估

| 风险项 | 等级 | 缓解措施 |
|--------|------|----------|
| **API 破坏性变更** | 🔴 高 | 逐行对照 changelog，编写迁移指南 |
| **DisplayHandle 生命周期** | 🔴 高 | 确保 EventLoop 生命周期 > Instance |
| **NVIDIA 驱动兼容性** | 🟡 中 | v28.0.1 已修复，需实测验证 |
| **winit 版本冲突** | 🟡 中 | 同步升级到 winit 0.30+ |
| **MSRV 不兼容** | 🟡 中 | 提前检查所有 crate 的 MSRV |
| **回归测试不足** | 🟡 中 | 建立自动化测试矩阵 |

---

## 🔄 回滚方案

如果升级后出现严重问题：

```bash
# 1. 恢复 Cargo.toml
git checkout HEAD -- Cargo.toml

# 2. 清理构建缓存
cargo clean

# 3. 重新构建
cargo build

# 4. 验证回滚
cargo run --example cli_player -- <test_video>
```

**代码回滚点**: 
- 提交前创建分支 `backup-before-wgpu-upgrade`
- 保留旧版 `wgpu_renderer.rs` 作为参考

---

## 📚 参考资源

- [wgpu v29.0.3 CHANGELOG](https://github.com/gfx-rs/wgpu/blob/v29.0.3/CHANGELOG.md)
- [wgpu Migration Guide](https://github.com/gfx-rs/wgpu/wiki/Migration-Guide)
- [wgpu Examples](https://github.com/gfx-rs/wgpu/tree/v29.0.3/examples)
- [DX12 Agility SDK](https://devblogs.microsoft.com/directx/directx12agility/)

---

## 📈 工作量估算

| 阶段 | 预计时间 | 负责人 |
|------|----------|--------|
| 准备工作 | 0.5 天 | - |
| 核心 API 迁移 | 2-3 天 | 核心开发 |
| 测试验证 | 1-2 天 | QA + 开发 |
| 可选优化 | 1 天 | 核心开发 |
| **总计** | **4.5-7.5 天** | - |

---

## ✅ 验收标准

1. [ ] 所有平台编译通过（Windows/macOS/Linux）
2. [ ] 视频播放功能正常（YUV→RGB 转换正确）
3. [ ] 窗口关闭无崩溃
4. [ ] NVIDIA GPU 上无多线程崩溃
5. [ ] 性能不低于升级前（±5% 容差）
6. [ ] 内存占用无明显增长
7. [ ] 通过所有现有单元测试

---

**文档版本**: v2.0  
**更新日期**: 2026-06-10  
**基于 wgpu**: 29.0.3 (2026-05-02 发布)
