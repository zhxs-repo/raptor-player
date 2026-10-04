/// High-level Dart API for the Raptor Player.
///
/// Wraps the low-level [RaptorBindings] with idiomatic Dart types,
/// async/await, and [Stream]-based event delivery.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'raptor_bindings.dart';
import 'raptor_commands.dart';
import 'raptor_events.dart';

/// Exception thrown when a Raptor command fails.
class RaptorException implements Exception {
  RaptorException(this.code, this.message);

  final int code;
  final String message;

  @override
  String toString() =>
      'RaptorException(${RaptorErrorCode.name(code)}): $message';
}

/// High-level client for the Raptor Player.
///
/// Usage:
/// ```dart
/// final client = RaptorClient(bindings);
/// await client.loadFile('/path/to/video.mp4');
/// await client.play();
///
/// client.events.listen((event) {
///   if (event is FileLoadedEvent) {
///     print('Duration: ${event.duration}s');
///   }
/// });
///
/// // ... later
/// await client.stop();
/// client.dispose();
/// ```
class RaptorClient {
  /// Create a new player client with the given [bindings].
  RaptorClient(this._bindings) {
    _handle = _bindings.create();
    if (_handle == nullptr) {
      throw StateError('raptor_create returned null handle');
    }
    _startEventPolling();
  }

  final RaptorBindings _bindings;
  late final Pointer<RaptorHandle> _handle;
  bool _disposed = false;

  // Event stream
  final StreamController<RaptorEvent> _eventController =
      StreamController<RaptorEvent>.broadcast();
  Timer? _pollTimer;

  // Property observers: observerId → property name
  final Map<int, String> _observers = {};

  /// Whether this client has been disposed.
  bool get isDisposed => _disposed;

  // ═══════════════════════════════════════════════════
  // Event stream
  // ═══════════════════════════════════════════════════

  /// Stream of player events.
  ///
  /// Events are polled from the native layer every 16ms (~60Hz).
  Stream<RaptorEvent> get events => _eventController.stream;

  void _startEventPolling() {
    _pollTimer = Timer.periodic(const Duration(milliseconds: 16), (_) {
      _pollEvents();
    });
  }

  void _pollEvents() {
    if (_disposed) return;

    // Poll all pending events
    while (true) {
      final ptr = _bindings.pollEvent(_handle);
      if (ptr == nullptr) break;

      try {
        final json = ptr.toDartString();
        final event = RaptorEvent.parse(json);
        _eventController.add(event);

        // End event stops polling
        if (event is EndEvent) {
          _pollTimer?.cancel();
          _pollTimer = null;
        }
      } finally {
        _bindings.freeString(ptr);
      }
    }
  }

  // ═══════════════════════════════════════════════════
  // Commands
  // ═══════════════════════════════════════════════════

  /// Load a media file for playback.
  ///
  /// Returns media info via the [FileLoadedEvent] on the event stream.
  void loadFile(String url) => _sendCommand(RaptorCommands.loadFile(url));

  /// Start playback.
  void play() => _sendCommand(RaptorCommands.play);

  /// Pause playback.
  void pause() => _sendCommand(RaptorCommands.pause);

  /// Toggle play/pause.
  void togglePause() => _sendCommand(RaptorCommands.togglePause);

  /// Stop playback.
  void stop() => _sendCommand(RaptorCommands.stop);

  /// Seek to [position] (Duration from start).
  void seek(Duration position) {
    _sendCommand(
      RaptorCommands.seek(position.inMilliseconds / 1000.0),
    );
  }

  /// Set volume (0–100).
  void setVolume(int volume) =>
      _sendCommand(RaptorCommands.setVolume(volume));

  /// Mute or unmute. The stored volume is untouched.
  void setMute(bool muted) => _sendCommand(RaptorCommands.setMute(muted));

  /// Load an external subtitle file (SRT/ASS/SSA).
  void loadSubtitle(String path) =>
      _sendCommand(RaptorCommands.loadSubtitle(path));

  /// Toggle subtitle visibility.
  void toggleSubtitle() => _sendCommand(RaptorCommands.toggleSubtitle);

  /// Load a danmaku file (Bilibili XML / JSON).
  void loadDanmaku(String path) =>
      _sendCommand(RaptorCommands.loadDanmaku(path));

  /// Toggle danmaku visibility.
  void toggleDanmaku() => _sendCommand(RaptorCommands.toggleDanmaku);

  /// Set danmaku opacity (0–100).
  void setDanmakuOpacity(int opacity) =>
      _sendCommand(RaptorCommands.setDanmakuOpacity(opacity));

  /// Quit the player.
  void quit() => _sendCommand(RaptorCommands.quit);

  void _sendCommand(String cmdJson) {
    if (_disposed) {
      throw StateError('RaptorClient has been disposed');
    }

    final cmdPtr = cmdJson.toNativeUtf8();
    try {
      final result = _bindings.command(_handle, cmdPtr);
      if (result != RaptorErrorCode.ok) {
        final errMsg = _getLastError();
        throw RaptorException(result, errMsg);
      }
    } finally {
      calloc.free(cmdPtr);
    }
  }

  // ═══════════════════════════════════════════════════
  // Properties
  // ═══════════════════════════════════════════════════

  /// Read a property by name. Returns the raw JSON string or null.
  String? getProperty(String name) {
    if (_disposed) return null;

    final namePtr = name.toNativeUtf8();
    try {
      final result = _bindings.getProperty(_handle, namePtr);
      if (result == nullptr) return null;
      try {
        return result.toDartString();
      } finally {
        _bindings.freeString(result);
      }
    } finally {
      calloc.free(namePtr);
    }
  }

  /// Read a property as a parsed JSON value.
  Object? getPropertyJson(String name) {
    final raw = getProperty(name);
    if (raw == null) return null;
    try {
      return jsonDecode(raw);
    } catch (_) {
      return raw;
    }
  }

  /// Read the current playback position in seconds.
  double get position {
    final val = getPropertyJson('position');
    if (val is num) return val.toDouble();
    return 0.0;
  }

  /// Read the current volume (0–100).
  int get volume {
    final val = getPropertyJson('volume');
    if (val is int) return val;
    return 100;
  }

  /// Set a property by name with a JSON value.
  void setProperty(String name, Object? value) {
    if (_disposed) return;

    final namePtr = name.toNativeUtf8();
    final valueStr = jsonEncode(value);
    final valuePtr = valueStr.toNativeUtf8();
    try {
      _bindings.setProperty(_handle, namePtr, valuePtr);
    } finally {
      calloc.free(namePtr);
      calloc.free(valuePtr);
    }
  }

  // ═══════════════════════════════════════════════════
  // Surface management (Android / embedded)
  // ═══════════════════════════════════════════════════

  /// Set the external surface for video rendering (Android: ANativeWindow*).
  ///
  /// [nativeWindow] is the platform native window pointer (e.g. ANativeWindow*)
  /// cast to an integer. [nativeDisplay] is the platform display connection
  /// (Android: unused, pass 0). [width] and [height] are the surface dimensions
  /// in pixels.
  ///
  /// Can be called before or after [loadFile]:
  /// - Before: surface is stored and used when loadFile creates the renderer
  /// - After: hot-swaps the renderer via command channel
  void setSurface({
    required int nativeWindow,
    int nativeDisplay = 0,
    required int width,
    required int height,
  }) {
    if (_disposed) {
      throw StateError('RaptorClient has been disposed');
    }
    final result = _bindings.setSurface(
      _handle,
      nativeWindow,
      nativeDisplay,
      width,
      height,
    );
    if (result != RaptorErrorCode.ok) {
      throw RaptorException(result, _getLastError());
    }
  }

  /// Detach the current surface (Android onPause: surface destroyed).
  ///
  /// The render thread stops presenting frames but decoding continues.
  /// Audio keeps playing; video frames are silently dropped.
  void detachSurface() {
    if (_disposed) return;
    final result = _bindings.detachSurface(_handle);
    if (result != RaptorErrorCode.ok) {
      throw RaptorException(result, _getLastError());
    }
  }

  /// Resize the current surface (screen rotation, etc.).
  void resizeSurface({required int width, required int height}) {
    if (_disposed) return;
    final result = _bindings.resizeSurface(_handle, width, height);
    if (result != RaptorErrorCode.ok) {
      throw RaptorException(result, _getLastError());
    }
  }

  // ═══════════════════════════════════════════════════
  // Texture ID (for Flutter TextureWidget)
  // ═══════════════════════════════════════════════════

  /// Get the GPU texture ID for the current video frame.
  ///
  /// Returns -1 if no video is active.
  int get textureId {
    if (_disposed) return -1;
    return _bindings.getTextureId(_handle);
  }

  // ═══════════════════════════════════════════════════
  // Error info
  // ═══════════════════════════════════════════════════

  String _getLastError() {
    final ptr = _bindings.lastError(_handle);
    if (ptr == nullptr) return 'unknown error';
    try {
      return ptr.toDartString();
    } finally {
      _bindings.freeString(ptr);
    }
  }

  /// Get the last error message, or null if no error.
  String? get lastError {
    if (_disposed) return null;
    final ptr = _bindings.lastError(_handle);
    if (ptr == nullptr) return null;
    try {
      return ptr.toDartString();
    } finally {
      _bindings.freeString(ptr);
    }
  }

  // ═══════════════════════════════════════════════════
  // Lifecycle
  // ═══════════════════════════════════════════════════

  /// Dispose the player and release all resources.
  void dispose() {
    if (_disposed) return;
    _disposed = true;

    _pollTimer?.cancel();
    _pollTimer = null;

    // Clean up property observers
    for (final observerId in _observers.keys) {
      _bindings.unobserveProperty(_handle, observerId);
    }
    _observers.clear();

    // Destroy native handle
    _bindings.destroy(_handle);

    // Close event stream
    if (!_eventController.isClosed) {
      _eventController.close();
    }
  }
}
