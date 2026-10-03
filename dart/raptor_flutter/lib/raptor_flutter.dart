/// Raptor Flutter Plugin — Android SurfaceView integration for Raptor Player.
///
/// Provides [RaptorVideoView] widget that renders video via Android
/// `SurfaceView` + `ANativeWindow` FFI, and handles Activity lifecycle
/// (pause/resume surface on background/foreground).
///
/// ## Usage
///
/// ```dart
/// import 'package:raptor_flutter/raptor_flutter.dart';
///
/// class PlayerScreen extends StatefulWidget { ... }
///
/// class _PlayerScreenState extends State<PlayerScreen> {
///   late final RaptorClient client;
///
///   @override
///   void initState() {
///     super.initState();
///     client = RaptorClient(RaptorBindings.load());
///   }
///
///   @override
///   Widget build(BuildContext context) {
///     return RaptorVideoView(client: client);
///   }
/// }
/// ```
library;

export 'package:raptor_player/raptor_player.dart';

export 'src/raptor_video_view.dart' show RaptorVideoView;
