# raptor_flutter example

Runnable Android host for the Raptor Player kernel: plays a video on a
`SurfaceView`, and overlays ASS subtitles and danmaku on top.

## 首次准备

```powershell
# 1. FFmpeg for Android（只需一次，~29 MB 静态库）
sh ../../scripts/build_ffmpeg_android.sh

# 2. libraptor_ffi.so → jniLibs（cargo 产物 + 拷贝一步到位）
sh ../../scripts/build_android_lib.sh
```

## 运行

直接跑：

```powershell
cd dart/raptor_flutter/example
flutter run
```

界面上有三个按钮：**Video / Subtitle / Danmaku**，点开就是系统文件选择器
（SAF），选完立刻生效。想跳过交互做自动化验证时，仍然可以用
`--dart-define` 直接喂路径：

```powershell
flutter run --dart-define=RAPTOR_VIDEO=<video.mp4> `
           --dart-define=RAPTOR_SUBTITLE=<001.ass> `
           --dart-define=RAPTOR_DANMAKU=<danmaku.xml>
```

### 关于"选文件"这件事

`RaptorPlayer.loadFile` 走 `std::path::Path::exists` 预检，然后让 FFmpeg
按**普通文件路径**打开。所以 SAF 直接回传的 `content://` URI 两样都不满足。

插件的做法是：`pickFile` 走 SAF 拿到 URI，**把文件复制到 app 私有目录**
（`filesDir/picked/`），再把那个副本的真实路径回传给 Dart：

```dart
final f = await RaptorFilePicker.pickFile(mimeTypes: ['video/*']);
if (f != null) client.loadFile(f.path);   // f.path 是真实可读路径
```

几个取舍：

- **视频限定 `video/*`**，字幕/弹幕不限（`*/*`）。`.ass` / `.xml` 在 Android 的
  MIME 表里没有正经类型，限定了文件管理器就直接看不见它们；改由 Dart 侧按扩展名校验。
- **复制而不是持有 URI 授权**：SAF 给的是临时读权限，URI 随时可能失效，
  而这个路径要交给 FFmpeg 长时间打开。落在 `filesDir` 而不是 `cacheDir`，
  后者会被系统清掉。
- **刻意不加超时**：人在目录里翻找几分钟是常事，任何时限都会把正常操作判成失败。
  真出现失联（Dart 侧按钮卡死）时，再点一次会被新的一次选择顶掉
  （`PICK_SUPERSEDED`），所以不会卡死。

### 路径怎么选

不想用选择器时，`--dart-define` 只能传 app 私有目录里的绝对路径：

- **app 私有目录不需要任何权限**，最省事。用 `adb push` 放进去：
  ```powershell
  adb push test_video.mp4 /data/local/tmp/
  adb shell run-as dev.dylplayer.raptor_example mkdir -p files
  adb shell run-as dev.dylplayer.raptor_example cp /data/local/tmp/test_video.mp4 files/
  # 然后 --dart-define=RAPTOR_VIDEO=/data/user/0/dev.dylplayer.raptor_example/files/test_video.mp4
  ```
- `/sdcard/...` 需要用户授予存储权限。manifest 里已经声明了
  `READ_MEDIA_VIDEO` / `READ_MEDIA_AUDIO`，但**本 example 不主动弹权限框** ——
  授了权就能用。
- `content://` URI 和 http(s) 地址**不支持**：`load_file` 只接受本地存在的路径。
  网络播放需要改 Rust 侧的前置检查。

## 界面

- **Video / Subtitle / Danmaku** — 分别选视频、字幕（`.ass/.ssa/.srt/.vtt/.sub`）、
  弹幕（`.xml/.json`）文件
- 进度条拖动 = `seek`
- 右上角两个图标分别切换字幕 / 弹幕的显示
- 状态栏显示当前的编解码信息与错误信息

## 排查

Rust 侧日志走 logcat：

```powershell
adb logcat -s raptor
```

Dart 侧日志（surface attach / detach 之类）：

```powershell
adb logcat | Select-String "RaptorVideoView|flutter"
```

关键日志点：

| 日志 | 含义 |
|---|---|
| `surface attached (W x H, nativeWindow=0x...)` | Surface 已交给渲染线程 |
| `native reported surface destroyed, detaching` | 系统回收了 surface，正常路径 |
| `ExtRenderState initialized: WxH, format=..., present=...` | wgpu surface 创建成功（Vulkan/GLES 后端） |
| `no suitable GPU adapter` | 设备上没有可用的 Vulkan/GLES 驱动 |
| `load_system_font: no system font found` | Android 字体路径没命中，字幕/弹幕会是空的 |

如果画面全黑且没有 `surface attached`，先确认
`jniLibs/arm64-v8a/libraptor_ffi.so` 存在：

```powershell
Get-ChildItem android\app\src\main\jniLibs\arm64-v8a
```

## 已知限制

- 只打包 `arm64-v8a`（`abiFilters`）。32 位设备上装不上，这是有意的：
  没有对应的 `.so`，`DynamicLibrary.open` 必然失败。
- 真机 16KB page size（Android 15+）：NDK r28 默认按 16KB 对齐，理论上没问题，
  但未在 16KB 设备上验证过。
