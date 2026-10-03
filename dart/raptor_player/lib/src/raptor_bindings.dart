/// Low-level dart:ffi bindings for libraptor (raptor.h)
///
/// This file maps 1:1 to the C ABI functions exported by raptor-ffi.
/// Users should prefer the high-level [RaptorClient] API instead.
library;

import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

// ═══════════════════════════════════════════════════
// Native types
// ═══════════════════════════════════════════════════

/// Opaque handle — Dart side only sees a pointer.
final class RaptorHandle extends Opaque {}

/// Native callback signatures
typedef RaptorEventCallbackNative = Void Function(
  Pointer<Utf8> eventJson,
  Pointer<Void> userData,
);
typedef RaptorPropertyCallbackNative = Void Function(
  Pointer<Utf8> valueJson,
  Pointer<Void> userData,
);

// ═══════════════════════════════════════════════════
// FFI function typedefs
// ═══════════════════════════════════════════════════

// RaptorHandle* raptor_create(void)
typedef _RaptorCreateNative = Pointer<RaptorHandle> Function();
typedef _RaptorCreateDart = Pointer<RaptorHandle> Function();

// void raptor_destroy(RaptorHandle* handle)
typedef _RaptorDestroyNative = Void Function(Pointer<RaptorHandle>);
typedef _RaptorDestroyDart = void Function(Pointer<RaptorHandle>);

// int raptor_command(RaptorHandle* handle, const char* cmd_json)
typedef _RaptorCommandNative = Int32 Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
);
typedef _RaptorCommandDart = int Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
);

// char* raptor_get_property(RaptorHandle* handle, const char* name)
typedef _RaptorGetPropertyNative = Pointer<Utf8> Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
);
typedef _RaptorGetPropertyDart = Pointer<Utf8> Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
);

// int raptor_set_property(RaptorHandle*, const char*, const char*)
typedef _RaptorSetPropertyNative = Int32 Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
  Pointer<Utf8>,
);
typedef _RaptorSetPropertyDart = int Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
  Pointer<Utf8>,
);

// int64_t raptor_observe_property(RaptorHandle*, const char*, callback, void*)
typedef _RaptorObservePropertyNative = Int64 Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
  Pointer<NativeFunction<RaptorPropertyCallbackNative>>,
  Pointer<Void>,
);
typedef _RaptorObservePropertyDart = int Function(
  Pointer<RaptorHandle>,
  Pointer<Utf8>,
  Pointer<NativeFunction<RaptorPropertyCallbackNative>>,
  Pointer<Void>,
);

// int raptor_unobserve_property(RaptorHandle*, int64_t)
typedef _RaptorUnobservePropertyNative = Int32 Function(
  Pointer<RaptorHandle>,
  Int64,
);
typedef _RaptorUnobservePropertyDart = int Function(
  Pointer<RaptorHandle>,
  int,
);

// void raptor_set_event_callback(RaptorHandle*, callback, void*)
typedef _RaptorSetEventCallbackNative = Void Function(
  Pointer<RaptorHandle>,
  Pointer<NativeFunction<RaptorEventCallbackNative>>,
  Pointer<Void>,
);
typedef _RaptorSetEventCallbackDart = void Function(
  Pointer<RaptorHandle>,
  Pointer<NativeFunction<RaptorEventCallbackNative>>,
  Pointer<Void>,
);

// char* raptor_poll_event(RaptorHandle*)
typedef _RaptorPollEventNative = Pointer<Utf8> Function(Pointer<RaptorHandle>);
typedef _RaptorPollEventDart = Pointer<Utf8> Function(Pointer<RaptorHandle>);

// int64_t raptor_get_texture_id(RaptorHandle*)
typedef _RaptorGetTextureIdNative = Int64 Function(Pointer<RaptorHandle>);
typedef _RaptorGetTextureIdDart = int Function(Pointer<RaptorHandle>);

// int32_t raptor_set_surface(RaptorHandle*, int64_t, int64_t, uint32_t, uint32_t)
typedef _RaptorSetSurfaceNative = Int32 Function(
  Pointer<RaptorHandle>,
  Int64,
  Int64,
  Uint32,
  Uint32,
);
typedef _RaptorSetSurfaceDart = int Function(
  Pointer<RaptorHandle>,
  int,
  int,
  int,
  int,
);

// int32_t raptor_detach_surface(RaptorHandle*)
typedef _RaptorDetachSurfaceNative = Int32 Function(Pointer<RaptorHandle>);
typedef _RaptorDetachSurfaceDart = int Function(Pointer<RaptorHandle>);

// int32_t raptor_resize_surface(RaptorHandle*, uint32_t, uint32_t)
typedef _RaptorResizeSurfaceNative = Int32 Function(
  Pointer<RaptorHandle>,
  Uint32,
  Uint32,
);
typedef _RaptorResizeSurfaceDart = int Function(
  Pointer<RaptorHandle>,
  int,
  int,
);

// void raptor_free_string(char*)
typedef _RaptorFreeStringNative = Void Function(Pointer<Utf8>);
typedef _RaptorFreeStringDart = void Function(Pointer<Utf8>);

// char* raptor_last_error(RaptorHandle*)
typedef _RaptorLastErrorNative = Pointer<Utf8> Function(Pointer<RaptorHandle>);
typedef _RaptorLastErrorDart = Pointer<Utf8> Function(Pointer<RaptorHandle>);

// ═══════════════════════════════════════════════════
// Bindings class
// ═══════════════════════════════════════════════════

/// Low-level bindings to libraptor / raptor_ffi.dll / libraptor.so.
///
/// Usage:
/// ```dart
/// final bindings = RaptorBindings.load();
/// final handle = bindings.create();
/// ```
class RaptorBindings {
  RaptorBindings._(this._lib);

  final DynamicLibrary _lib;

  // Lazily resolved function pointers
  late final _RaptorCreateDart create =
      _lib.lookupFunction<_RaptorCreateNative, _RaptorCreateDart>(
    'raptor_create',
  );

  late final _RaptorDestroyDart destroy =
      _lib.lookupFunction<_RaptorDestroyNative, _RaptorDestroyDart>(
    'raptor_destroy',
  );

  late final _RaptorCommandDart command =
      _lib.lookupFunction<_RaptorCommandNative, _RaptorCommandDart>(
    'raptor_command',
  );

  late final _RaptorGetPropertyDart getProperty =
      _lib.lookupFunction<_RaptorGetPropertyNative, _RaptorGetPropertyDart>(
    'raptor_get_property',
  );

  late final _RaptorSetPropertyDart setProperty =
      _lib.lookupFunction<_RaptorSetPropertyNative, _RaptorSetPropertyDart>(
    'raptor_set_property',
  );

  late final _RaptorObservePropertyDart observeProperty = _lib
      .lookupFunction<_RaptorObservePropertyNative, _RaptorObservePropertyDart>(
    'raptor_observe_property',
  );

  late final _RaptorUnobservePropertyDart unobserveProperty = _lib
      .lookupFunction<
        _RaptorUnobservePropertyNative,
        _RaptorUnobservePropertyDart
      >('raptor_unobserve_property');

  late final _RaptorSetEventCallbackDart setEventCallback = _lib
      .lookupFunction<
        _RaptorSetEventCallbackNative,
        _RaptorSetEventCallbackDart
      >('raptor_set_event_callback');

  late final _RaptorPollEventDart pollEvent =
      _lib.lookupFunction<_RaptorPollEventNative, _RaptorPollEventDart>(
    'raptor_poll_event',
  );

  late final _RaptorGetTextureIdDart getTextureId =
      _lib.lookupFunction<_RaptorGetTextureIdNative, _RaptorGetTextureIdDart>(
    'raptor_get_texture_id',
  );

  late final _RaptorSetSurfaceDart setSurface =
      _lib.lookupFunction<_RaptorSetSurfaceNative, _RaptorSetSurfaceDart>(
    'raptor_set_surface',
  );

  late final _RaptorDetachSurfaceDart detachSurface =
      _lib.lookupFunction<_RaptorDetachSurfaceNative, _RaptorDetachSurfaceDart>(
    'raptor_detach_surface',
  );

  late final _RaptorResizeSurfaceDart resizeSurface =
      _lib.lookupFunction<_RaptorResizeSurfaceNative, _RaptorResizeSurfaceDart>(
    'raptor_resize_surface',
  );

  late final _RaptorFreeStringDart freeString =
      _lib.lookupFunction<_RaptorFreeStringNative, _RaptorFreeStringDart>(
    'raptor_free_string',
  );

  late final _RaptorLastErrorDart lastError =
      _lib.lookupFunction<_RaptorLastErrorNative, _RaptorLastErrorDart>(
    'raptor_last_error',
  );

  /// Load the native library from the default platform path.
  ///
  /// - Windows: raptor_ffi.dll
  /// - macOS:   libraptor.dylib
  /// - Linux:   libraptor.so
  factory RaptorBindings.load([String? path]) {
    final lib = DynamicLibrary.open(path ?? _defaultLibPath());
    return RaptorBindings._(lib);
  }

  /// Load from the current process (for statically linked scenarios).
  factory RaptorBindings.loadFromProcess() {
    return RaptorBindings._(DynamicLibrary.process());
  }

  static String _defaultLibPath() {
    if (Platform.isWindows) return 'raptor_ffi.dll';
    if (Platform.isMacOS) return 'libraptor.dylib';
    if (Platform.isLinux) return 'libraptor.so';
    if (Platform.isAndroid) return 'libraptor.so';
    if (Platform.isIOS) return 'RaptorPlayer'; // framework
    throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');
  }
}

// ═══════════════════════════════════════════════════
// Error codes (mirror Rust ErrorCode enum)
// ═══════════════════════════════════════════════════

/// Error codes returned by raptor_command.
abstract class RaptorErrorCode {
  static const int ok = 0;
  static const int invalidArgument = -1;
  static const int invalidState = -2;
  static const int fileNotFound = -3;
  static const int demuxError = -4;
  static const int decodeError = -5;
  static const int renderError = -6;
  static const int audioError = -7;
  static const int pipelineError = -8;
  static const int internal = -99;

  /// Convert an error code to a human-readable name.
  static String name(int code) {
    return switch (code) {
      ok => 'Ok',
      invalidArgument => 'InvalidArgument',
      invalidState => 'InvalidState',
      fileNotFound => 'FileNotFound',
      demuxError => 'DemuxError',
      decodeError => 'DecodeError',
      renderError => 'RenderError',
      audioError => 'AudioError',
      pipelineError => 'PipelineError',
      internal => 'Internal',
      _ => 'Unknown($code)',
    };
  }
}
