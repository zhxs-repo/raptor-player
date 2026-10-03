/// Raptor Player — Dart FFI bindings for the cross-platform media player kernel.
///
/// ## Quick Start
///
/// ```dart
/// import 'package:raptor_player/raptor_player.dart';
///
/// void main() async {
///   final bindings = RaptorBindings.load();
///   final client = RaptorClient(bindings);
///
///   client.events.listen((event) {
///     switch (event) {
///       case FileLoadedEvent(:final duration):
///         print('Loaded: ${duration}s');
///       case ErrorEvent(:final message):
///         print('Error: $message');
///       case EndEvent():
///         print('Playback ended');
///       default:
///         break;
///     }
///   });
///
///   client.loadFile('/path/to/video.mp4');
///   client.play();
///
///   // ... later
///   client.dispose();
/// }
/// ```
library;

export 'src/raptor_bindings.dart'
    show RaptorBindings, RaptorHandle, RaptorErrorCode;
export 'src/raptor_client.dart' show RaptorClient, RaptorException;
export 'src/raptor_commands.dart' show RaptorCommands, SeekMode;
export 'src/raptor_events.dart';
