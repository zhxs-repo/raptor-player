/// Raptor Player Android example.
///
/// Demonstrates the full path: load a video, play it on a SurfaceView driven by
/// the Rust renderer, then overlay ASS subtitles and danmaku on top.
///
/// The media path is passed on the command line so the same build can be used
/// for any file:
///
///   flutter run --dart-define=RAPTOR_VIDEO=/sdcard/Movies/test.mp4 \
///               --dart-define=RAPTOR_SUBTITLE=/sdcard/Movies/001.ass \
///               --dart-define=RAPTOR_DANMAKU=/sdcard/Movies/danmaku.xml
///
/// Or copy files into the app's private storage (no runtime permission needed)
/// and pass e.g. /data/user/0/dev.dylplayer.raptor_example/files/test.mp4
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:raptor_flutter/raptor_flutter.dart';

void main() {
  runApp(const RaptorExampleApp());
}

class RaptorExampleApp extends StatelessWidget {
  const RaptorExampleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Raptor Player',
      theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
      home: const PlayerPage(),
    );
  }
}

class PlayerPage extends StatefulWidget {
  const PlayerPage({super.key});

  @override
  State<PlayerPage> createState() => _PlayerPageState();
}

class _PlayerPageState extends State<PlayerPage> {
  /// 来自 --dart-define 的初始路径（便于自动化验证）；用户用选择器选过之后
  /// 以 [_videoPathOverride] 为准。
  static const _videoFromEnv = String.fromEnvironment('RAPTOR_VIDEO');
  static const _subtitlePath = String.fromEnvironment('RAPTOR_SUBTITLE');
  static const _danmakuPath = String.fromEnvironment('RAPTOR_DANMAKU');

  String _videoPathOverride = '';
  String get _videoPath =>
      _videoPathOverride.isNotEmpty ? _videoPathOverride : _videoFromEnv;

  /// surface 是否已交给渲染线程（由 RaptorVideoView 回报）。
  bool _surfaceReady = false;

  /// 正在拖动进度条时的临时位置（毫秒）；null = 没在拖动
  ///
  /// 非 null 时进度条和时间标签都用它，不跟播放位置同步 —— 否则会和
  /// [_ticker] 抢控制权，滑块在手指底下反复回弹。
  double? _scrubbing;

  RaptorClient? _client;
  String? _error;
  String _status = 'initializing';
  Duration _position = Duration.zero;
  Duration _duration = Duration.zero;
  bool _subtitleOn = false;
  bool _danmakuOn = false;
  bool _autoLoaded = false;
  bool _picking = false;
  String _videoLabel = '';
  String _subtitleLabel = '';
  String _danmakuLabel = '';
  Timer? _ticker;

  @override
  void initState() {
    super.initState();
    _init();
  }

  void _init() {
    try {
      final client = RaptorClient(RaptorBindings.load());
      _client = client;
      // Attach the surface before loadFile: the player stores the pending
      // window and creates its ExternalRenderer as soon as media is opened.
      client.events.listen(_onEvent);
      setState(() => _status = 'loaded bindings, waiting for surface');
    } on RaptorException catch (e) {
      setState(() {
        _error = 'Failed to load libraptor_ffi.so: $e';
        _status = 'error';
      });
    }
  }

  void _onEvent(RaptorEvent event) {
    if (!mounted) return;
    setState(() {
      switch (event) {
        case FileLoadedEvent(:final duration, :final video, :final audio):
          _duration = Duration(milliseconds: (duration * 1000).round());
          _status = 'loaded ${video?.codec ?? 'no video'} / '
              '${audio?.codec ?? 'no audio'}  ${_duration.inSeconds}s';
          _loadOverlays();
          _client?.play();
        case ErrorEvent(:final message):
          _error = message;
          _status = 'error';
        case SeekEvent(:final to):
          _position = Duration(milliseconds: (to * 1000).round());
        case EndFileEvent():
          _status = 'file ended';
        case PlaybackRestartEvent():
          _status = 'playing';
        case EndEvent():
          _status = 'stopped';
        default:
          break;
      }
    });
  }

  void _loadOverlays() {
    final client = _client;
    if (client == null) return;
    if (_subtitlePath.isNotEmpty && File(_subtitlePath).existsSync()) {
      client.loadSubtitle(_subtitlePath);
      setState(() {
        _subtitleOn = true;
        _subtitleLabel = _subtitlePath.split('/').last;
      });
    }
    if (_danmakuPath.isNotEmpty && File(_danmakuPath).existsSync()) {
      client.loadDanmaku(_danmakuPath);
      setState(() {
        _danmakuOn = true;
        _danmakuLabel = _danmakuPath.split('/').last;
      });
    }
  }

  /// 通过系统文件选择器选视频。
  ///
  /// 选中的文件由插件复制到 app 私有目录，回传真实路径 —— `content://` URI
  /// 过不了 Rust 侧 `Path::exists()` 那一关。
  Future<void> _pickVideo() async {
    if (_picking) return;
    setState(() => _picking = true);
    try {
      final picked = await RaptorFilePicker.pickFile(
        mimeTypes: const ['video/*'],
      );
      if (picked == null || !mounted) return; // 用户取消
      setState(() {
        _videoPathOverride = picked.path;
        _videoLabel = picked.name;
        _error = null;
        _autoLoaded = false; // 等 surface 就绪后由 _onSurfaceReady 触发
        _status = _surfaceReady
            ? 'picked ${picked.name}'
            : 'picked ${picked.name}, waiting for surface';
      });
      _playIfSurfaceReady();
    } catch (e) {
      // PlatformException = 原生侧报错；TimeoutException = 回包失联（见 pickFile）。
      if (mounted) setState(() => _error = 'pick video failed: $e');
    } finally {
      if (mounted) setState(() => _picking = false);
    }
  }

  Future<void> _pickSubtitle() async {
    try {
      final picked = await RaptorFilePicker.pickSubtitle();
      if (picked == null || !mounted) return;
      _client?.loadSubtitle(picked.path);
      setState(() {
        _subtitleOn = true;
        _subtitleLabel = picked.name;
      });
    } on FormatException catch (e) {
      setState(() => _error = e.message);
    } catch (e) {
      setState(() => _error = 'pick subtitle failed: $e');
    }
  }

  Future<void> _pickDanmaku() async {
    try {
      final picked = await RaptorFilePicker.pickDanmaku();
      if (picked == null || !mounted) return;
      _client?.loadDanmaku(picked.path);
      setState(() {
        _danmakuOn = true;
        _danmakuLabel = picked.name;
      });
    } on FormatException catch (e) {
      setState(() => _error = e.message);
    } catch (e) {
      setState(() => _error = 'pick danmaku failed: $e');
    }
  }

  /// 进度条滑块当前应显示的值（毫秒）
  double get _sliderValue {
    final max = _duration.inMilliseconds > 0 ? _duration.inMilliseconds : 1;
    if (_scrubbing != null) return _scrubbing!.clamp(0.0, max.toDouble());
    return _position.inMilliseconds.clamp(0, max).toDouble();
  }

  /// 进度条左侧的时间标签：拖动中显示目标位置，否则显示播放位置
  String get _positionLabel {
    if (_scrubbing != null) return _format(Duration(milliseconds: _scrubbing!.round()));
    return _format(_position);
  }

  void _seekTo(double ms) {
    final client = _client;
    if (client == null) return;
    try {
      client.seek(Duration(milliseconds: ms.round()));
      // 立即把本地位置推到目标值：seek 是异步生效的（要等 demux_loop 下一轮
      // 才消费请求），不预置的话松手的一瞬间进度条会先弹回旧位置再跳过去。
      setState(() => _position = Duration(milliseconds: ms.round()));
    } on RaptorException catch (e) {
      setState(() {
        _error = 'seek failed: $e';
        _scrubbing = null;
      });
    }
  }

  void _playIfSurfaceReady() {
    if (_surfaceReady) _load();
  }

  void _load() {
    if (_videoPath.isEmpty) {
      setState(() {
        _error = 'Pick a video file first';
        _status = 'surface ready, pick a video to start';
      });
      return;
    }
    try {
      _client?.loadFile(_videoPath);
      _startTicker();
      setState(() {
        _error = null;
        // 新文件从 0 开始，别留着上一部的进度
        _position = Duration.zero;
        _scrubbing = null;
        _status = 'loaded ${_videoLabel.isEmpty ? _videoPath : _videoLabel}';
      });
    } on RaptorException catch (e) {
      setState(() {
        _error = 'loadFile failed: $e';
        _status = 'error';
      });
    }
  }

  /// Surface 就绪后自动开始播放。
  ///
  /// 顺序很重要：必须等 raptor_set_surface 成功，否则 Rust 侧会因为拿不到
  /// 渲染目标而拒绝 load_file。
  void _onSurfaceReady() {
    _surfaceReady = true;
    // 初始化时那句 "waiting for surface" 到这里就该退休了。
    if (_status == 'loaded bindings, waiting for surface') {
      setState(() => _status = 'surface ready');
    }
    if (_autoLoaded) return;
    _autoLoaded = true;
    _load();
  }

  void _startTicker() {
    _ticker?.cancel();
    _ticker = Timer.periodic(const Duration(milliseconds: 200), (_) {
      final client = _client;
      if (client == null || !mounted) return;
      // 拖动中不要跟播放位置：seek 还没生效，真实位置还是旧的，
      // 每 200ms 把滑块拽回去一次正是"进度条乱跳"的来源。
      if (_scrubbing != null) return;
      // `position` is a double in seconds.
      final pos = client.position;
      final ms = (pos * 1000).round();
      // Only repaint on a visible change: every tick goes through the FFI
      // property store.
      if ((ms - _position.inMilliseconds).abs() >= 100) {
        setState(() => _position = Duration(milliseconds: ms));
      }
    });
  }

  @override
  void dispose() {
    _ticker?.cancel();
    _client?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final client = _client;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Raptor Player'),
        actions: [
          if (_subtitleOn)
            IconButton(
              tooltip: 'toggle subtitles',
              icon: Icon(_subtitleOn ? Icons.subtitles : Icons.subtitles_off),
              onPressed: () {
                client?.toggleSubtitle();
                setState(() => _subtitleOn = !_subtitleOn);
              },
            ),
          if (_danmakuOn)
            IconButton(
              tooltip: 'toggle danmaku',
              icon: Icon(_danmakuOn ? Icons.chat_bubble : Icons.chat_bubble_outline),
              onPressed: () {
                client?.toggleDanmaku();
                setState(() => _danmakuOn = !_danmakuOn);
              },
            ),
        ],
      ),
      body: Column(
        children: [
          Expanded(
            child: client == null
                ? const Center(child: CircularProgressIndicator())
                : RaptorVideoView(
                    client: client,
                    onSurfaceReady: _onSurfaceReady,
                    backgroundColor: Colors.black,
                    placeholder: const Center(
                      child: Text(
                        'Raptor video rendering requires Android',
                        style: TextStyle(color: Colors.white38),
                      ),
                    ),
                  ),
          ),
          _buildControls(client),
        ],
      ),
    );
  }

  Widget _buildControls(RaptorClient? client) {
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(_status, style: Theme.of(context).textTheme.bodySmall),
            if (_error != null)
              Text(
                _error!,
                style: const TextStyle(color: Colors.redAccent),
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
              ),
            const SizedBox(height: 8),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                FilledButton.icon(
                  onPressed: client == null || _picking ? null : _pickVideo,
                  icon: const Icon(Icons.folder_open),
                  label: Text(_videoLabel.isEmpty ? 'Video' : _short(_videoLabel)),
                ),
                const SizedBox(width: 8),
                IconButton.filled(
                  tooltip: 'play / pause',
                  onPressed: client == null ? null : () => client.togglePause(),
                  icon: const Icon(Icons.play_arrow),
                ),
                const SizedBox(width: 8),
                IconButton.filled(
                  tooltip: 'stop',
                  onPressed: client == null ? null : () => client.stop(),
                  icon: const Icon(Icons.stop),
                ),
              ],
            ),
            const SizedBox(height: 8),
            // 字幕 / 弹幕：各选一个文件。选完立即 load，播放中也能换。
            Row(
              children: [
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: client == null ? null : _pickSubtitle,
                    icon: const Icon(Icons.subtitles_outlined, size: 18),
                    label: Text(
                      _subtitleLabel.isEmpty
                          ? 'Subtitle'
                          : _short(_subtitleLabel),
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: client == null ? null : _pickDanmaku,
                    icon: const Icon(Icons.chat_bubble_outline, size: 18),
                    label: Text(
                      _danmakuLabel.isEmpty
                          ? 'Danmaku'
                          : _short(_danmakuLabel),
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                Text(_positionLabel),
                const Spacer(),
                Text(_format(_duration)),
              ],
            ),
            // 拖动时用 _scrubbing 里的值显示，**不**跟着 _position 回弹：
            // 定时器每 200ms 回报一次真实播放位置，seek 生效前它一直停在旧值，
            // 滑块就会在手指拖动的过程中一下一下跳回去。
            Slider(
              value: _sliderValue,
              max: _duration.inMilliseconds > 0
                  ? _duration.inMilliseconds.toDouble()
                  : 1,
              // 松手才真正 seek：onChanged 每像素都会触发，逐像素发 seek 会把
              // demuxer 打爆（一次拖动 = 几十次 seek），而 Flutter 侧的
              // position 回报根本追不上，看起来就是"进度条乱跳"。
              onChangeStart: client == null
                  ? null
                  : (v) => setState(() => _scrubbing = v),
              onChanged: client == null
                  ? null
                  : (v) => setState(() => _scrubbing = v),
              onChangeEnd: client == null
                  ? null
                  : (v) {
                      setState(() => _scrubbing = null);
                      _seekTo(v);
                    },
            ),
          ],
        ),
      ),
    );
  }

  /// 按钮上只放得下几个字符，文件名太长就截断。
  static String _short(String name, [int max = 14]) {
    if (name.length <= max) return name;
    final dot = name.lastIndexOf('.');
    final ext = dot > 0 ? name.substring(dot) : '';
    return '${name.substring(0, max - ext.length - 1)}…$ext';
  }

  static String _format(Duration d) {
    final m = d.inMinutes;
    final s = d.inSeconds % 60;
    final ms = (d.inMilliseconds % 1000) ~/ 100;
    return '$m:${s.toString().padLeft(2, '0')}.$ms';
  }
}
