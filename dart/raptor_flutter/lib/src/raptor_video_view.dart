/// RaptorVideoView — Android SurfaceView 集成 widget
///
/// 在 Android 上创建 `SurfaceView` PlatformView，获取 `ANativeWindow*` 指针，
/// 通过 FFI 传给 Rust 层的 `raptor_set_surface`，实现视频渲染。
///
/// 自动监听 App 生命周期：
/// - `AppLifecycleState.paused` → `client.detachSurface()`
/// - `AppLifecycleState.resumed` → 重新获取 Surface 并 `client.setSurface()`
library;

import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:raptor_player/raptor_player.dart';

/// 视频渲染 Widget — 在 Android 上显示 SurfaceView，其他平台显示占位。
class RaptorVideoView extends StatefulWidget {
  const RaptorVideoView({
    super.key,
    required this.client,
    this.backgroundColor = const Color(0xFF000000),
    this.placeholder,
  });

  /// Raptor 播放器客户端实例。
  final RaptorClient client;

  /// 视频区域背景色（Surface 未就绪时显示）。
  final Color backgroundColor;

  /// 非 Android 平台的占位 widget。
  final Widget? placeholder;

  @override
  State<RaptorVideoView> createState() => _RaptorVideoViewState();
}

class _RaptorVideoViewState extends State<RaptorVideoView>
    with WidgetsBindingObserver {
  static const _viewType = 'dev.dylplayer.raptor_flutter/surface_view';
  static const _channel = MethodChannel('dev.dylplayer.raptor_flutter');

  int? _viewId;
  int _surfaceWidth = 0;
  int _surfaceHeight = 0;
  bool _surfaceReady = false;
  int _surfaceGeneration = 0;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _surfaceGeneration = identityHashCode(this);
  }

  @override
  void didUpdateWidget(covariant RaptorVideoView oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.client != widget.client) {
      _detachCurrentSurface();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _detachCurrentSurface();
    _destroyPlatformView();
    super.dispose();
  }

  // ── Lifecycle ──────────────────────────────────────────────────

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.paused:
      case AppLifecycleState.hidden:
      case AppLifecycleState.inactive:
        // Surface 可能即将被销毁 — detach
        if (_surfaceReady) {
          _detachCurrentSurface();
        }
        break;
      case AppLifecycleState.resumed:
        // Activity 恢复 — 如果 PlatformView 还在，重新 attach
        if (_viewId != null && !_surfaceReady) {
          unawaited(_reattachSurface());
        }
        break;
      case AppLifecycleState.detached:
        _detachCurrentSurface();
        break;
    }
  }

  // ── PlatformView management ────────────────────────────────────

  void _handlePlatformViewCreated(int id) {
    if (!mounted) return;
    _viewId = id;
    unawaited(_attachSurface(id));
  }

  Future<void> _attachSurface(int viewId) async {
    if (!mounted || widget.client.isDisposed) return;

    try {
      final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'getNativeWindow',
        {'viewId': viewId, 'generation': _surfaceGeneration},
      );
      if (result == null || !mounted) return;

      final nativeWindow = result['nativeWindow'] as int? ?? 0;
      final width = result['width'] as int? ?? 0;
      final height = result['height'] as int? ?? 0;

      if (nativeWindow == 0) {
        debugPrint('RaptorVideoView: nativeWindow is null (surface not ready)');
        return;
      }

      _surfaceWidth = width;
      _surfaceHeight = height;
      _surfaceReady = true;

      widget.client.setSurface(
        nativeWindow: nativeWindow,
        width: width,
        height: height,
      );
      debugPrint(
        'RaptorVideoView: surface attached ($width x $height, '
        'nativeWindow=0x${nativeWindow.toRadixString(16)})',
      );
    } on PlatformException catch (e) {
      debugPrint('RaptorVideoView: attachSurface failed: ${e.message}');
    } on RaptorException catch (e) {
      debugPrint('RaptorVideoView: setSurface failed: $e');
    }
  }

  Future<void> _reattachSurface() async {
    final viewId = _viewId;
    if (viewId == null || !mounted) return;
    await _attachSurface(viewId);
  }

  void _detachCurrentSurface() {
    if (_surfaceReady && !widget.client.isDisposed) {
      try {
        widget.client.detachSurface();
        debugPrint('RaptorVideoView: surface detached');
      } on RaptorException catch (e) {
        debugPrint('RaptorVideoView: detachSurface failed: $e');
      }
    }
    _surfaceReady = false;
  }

  Future<void> _destroyPlatformView() async {
    final viewId = _viewId;
    if (viewId == null) return;
    _viewId = null;
    try {
      await _channel.invokeMethod('destroyView', {'viewId': viewId});
    } on PlatformException {
      // Ignore — view may already be destroyed
    }
  }

  // ── Build ──────────────────────────────────────────────────────

  @override
  Widget build(BuildContext context) {
    if (kIsWeb || !Platform.isAndroid) {
      return widget.placeholder ??
          ColoredBox(
            color: widget.backgroundColor,
            child: const Center(
              child: Text(
                'Raptor video rendering requires Android',
                style: TextStyle(color: Color(0xFF888888)),
              ),
            ),
          );
    }

    return ColoredBox(
      color: widget.backgroundColor,
      child: AndroidView(
        viewType: _viewType,
        layoutDirection: TextDirection.ltr,
        creationParamsCodec: const StandardMessageCodec(),
        creationParams: <String, Object?>{
          'generation': _surfaceGeneration,
        },
        onPlatformViewCreated: _handlePlatformViewCreated,
        hitTestBehavior: PlatformViewHitTestBehavior.opaque,
        gestureRecognizers: const <Factory<OneSequenceGestureRecognizer>>{},
      ),
    );
  }
}
