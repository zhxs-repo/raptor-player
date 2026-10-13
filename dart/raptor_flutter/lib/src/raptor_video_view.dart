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
    this.onSurfaceReady,
    this.backgroundColor = const Color(0xFF000000),
    this.placeholder,
  });

  /// Raptor 播放器客户端实例。
  final RaptorClient client;

  /// Surface 成功交给渲染线程后回调（每次重建 Surface 都会调用）。
  ///
  /// [RaptorClient.loadFile] 必须在 surface 就绪之后调用 —— 顺序反了 Rust 侧会返回
  /// `load_file called before raptor_set_surface`。用它来自动开始播放，而不必等
  /// 用户点按钮。
  final VoidCallback? onSurfaceReady;

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
  bool _surfaceReady = false;
  int _surfaceGeneration = 0;

  // View whose getNativeWindow request is in flight. A second request while one
  // is pending would race for the same ANativeWindow, and the loser would have
  // to give its reference back.
  int? _attachingViewId;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _surfaceGeneration = identityHashCode(this);
    // Android recreates the SurfaceView surface on its own (screen off/on,
    // activity recreation, split-screen) without any Flutter lifecycle change.
    // Without this the renderer would keep presenting into a dead ANativeWindow
    // and the video stays black after returning to the foreground.
    _channel.setMethodCallHandler(_handlePlatformEvent);
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
    _channel.setMethodCallHandler(null);
    _detachCurrentSurface();
    _destroyPlatformView();
    super.dispose();
  }

  // ── Native → Dart surface events ───────────────────────────────

  Future<void> _handlePlatformEvent(MethodCall call) async {
    if (call.method == 'onSurfaceDestroyed') {
      // The native window is about to be torn down. detachSurface() is
      // synchronous on the Rust side and only returns once the render thread
      // has released its ANativeWindow reference, so it is safe for the host to
      // destroy the window right after.
      debugPrint('RaptorVideoView: native reported surface destroyed, detaching');
      _detachCurrentSurface();
    } else if (call.method == 'onSurfaceCreated') {
      debugPrint('RaptorVideoView: native reported surface created');
      final viewId = _viewId;
      if (viewId != null && !_surfaceReady) {
        unawaited(_attachSurface(viewId));
      }
    } else if (call.method == 'onSurfaceResized') {
      final viewId = _viewId;
      if (viewId != null && _surfaceReady) {
        // Ask for the current size only. getNativeWindow also hands out a +1
        // ANativeWindow reference, which has to be given straight back since
        // this path only resizes the surface Rust already holds.
        int? borrowed;
        try {
          final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
            'getNativeWindow',
            {'viewId': viewId, 'generation': _surfaceGeneration},
          );
          borrowed = result?['nativeWindow'] as int? ?? 0;
          final width = result?['width'] as int? ?? 0;
          final height = result?['height'] as int? ?? 0;
          if (width > 0 && height > 0) {
            widget.client.resizeSurface(width: width, height: height);
            debugPrint('RaptorVideoView: resized to ${width}x$height');
          }
        } on PlatformException catch (e) {
          debugPrint('RaptorVideoView: resize failed: ${e.message}');
        } on RaptorException catch (e) {
          debugPrint('RaptorVideoView: resizeSurface failed: $e');
        } finally {
          if (borrowed != null && borrowed != 0) {
            await _releaseNativeWindow(borrowed);
          }
        }
      }
    }
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

    // Only one attach may be in flight; a concurrent surfaceCreated event
    // (or a resume while the first request is still pending) would otherwise
    // race for the same native window.
    if (_attachingViewId != null) return;
    _attachingViewId = viewId;

    int? nativeWindow;
    try {
      final result = await _channel.invokeMethod<Map<dynamic, dynamic>>(
        'getNativeWindow',
        {'viewId': viewId, 'generation': _surfaceGeneration},
      );
      // The reply is worthless if the view went away (or another attach won
      // the race) while we were waiting. Ownership is still ours, so it has to
      // be returned to the host rather than dropped.
      if (result == null || !mounted || _viewId != viewId || widget.client.isDisposed) {
        final orphan = result?['nativeWindow'] as int? ?? 0;
        if (orphan != 0) {
          await _releaseNativeWindow(orphan);
        }
        return;
      }

      final window = result['nativeWindow'] as int? ?? 0;
      nativeWindow = window;
      final width = result['width'] as int? ?? 0;
      final height = result['height'] as int? ?? 0;

      if (window == 0) {
        debugPrint('RaptorVideoView: nativeWindow is null (surface not ready)');
        return;
      }

      _surfaceReady = true;

      // Ownership transfer point: after this the render thread owns the window
      // and releases it on detach, so we must not release it ourselves.
      widget.client.setSurface(
        nativeWindow: window,
        width: width,
        height: height,
      );
      nativeWindow = null;
      debugPrint(
        'RaptorVideoView: surface attached ($width x $height, '
        'nativeWindow=0x${window.toRadixString(16)})',
      );
      widget.onSurfaceReady?.call();
    } on PlatformException catch (e) {
      debugPrint('RaptorVideoView: attachSurface failed: ${e.message}');
    } on RaptorException catch (e) {
      debugPrint('RaptorVideoView: setSurface failed: $e');
    } finally {
      // Any window we received but did not hand over has to go back, or the
      // SurfaceView runs out of native windows and stops getting new surfaces.
      if (nativeWindow != null && nativeWindow != 0) {
        await _releaseNativeWindow(nativeWindow);
      }
      _attachingViewId = null;
    }
  }

  /// 归还一个未交给 Rust 的 ANativeWindow 引用。
  Future<void> _releaseNativeWindow(int window) async {
    try {
      await _channel.invokeMethod<void>(
        'releaseNativeWindow',
        {'window': window},
      );
    } on PlatformException catch (e) {
      debugPrint('RaptorVideoView: releaseNativeWindow failed: ${e.message}');
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
