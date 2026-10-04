/// Command JSON builders matching the Rust `Command` enum (serde JSON).
///
/// Serde externally-tagged representation:
/// - Unit variants: `"Play"`, `"Pause"`, `"Stop"`, etc.
/// - Struct variants: `{"LoadFile": {"url": "..."}}`, etc.
library;

import 'dart:convert';

/// Command builders — each returns a JSON string ready for raptor_command().
class RaptorCommands {
  RaptorCommands._(); // static-only class

  /// Load a media file.
  static String loadFile(String url) {
    return jsonEncode({'LoadFile': {'url': url}});
  }

  /// Start playback.
  static String get play => '"Play"';

  /// Pause playback.
  static String get pause => '"Pause"';

  /// Toggle play/pause.
  static String get togglePause => '"TogglePause"';

  /// Stop playback.
  static String get stop => '"Stop"';

  /// Seek to [target] seconds.
  static String seek(double target, {SeekMode mode = SeekMode.absolute}) {
    return jsonEncode({
      'Seek': {'target': target, 'mode': mode.name},
    });
  }

  /// Set volume (0–100).
  static String setVolume(int volume) {
    assert(volume >= 0 && volume <= 100, 'volume must be 0-100');
    return jsonEncode({'SetVolume': {'volume': volume}});
  }

  /// Mute or unmute. The stored volume is untouched.
  static String setMute(bool muted) {
    return jsonEncode({'SetMute': {'muted': muted}});
  }

  /// Load an external subtitle file (SRT/ASS/SSA).
  static String loadSubtitle(String path) {
    return jsonEncode({'LoadSubtitle': {'path': path}});
  }

  /// Toggle subtitle visibility.
  static String get toggleSubtitle => '"ToggleSubtitle"';

  /// Load a danmaku file (Bilibili XML / JSON).
  static String loadDanmaku(String path) {
    return jsonEncode({'LoadDanmaku': {'path': path}});
  }

  /// Toggle danmaku visibility.
  static String get toggleDanmaku => '"ToggleDanmaku"';

  /// Set danmaku opacity (0–100).
  static String setDanmakuOpacity(int opacity) {
    assert(opacity >= 0 && opacity <= 100, 'opacity must be 0-100');
    return jsonEncode({'SetDanmakuOpacity': {'opacity': opacity}});
  }

  /// Quit the player.
  static String get quit => '"Quit"';
}

/// Seek mode.
enum SeekMode {
  absolute,
  relative;

  String get name => switch (this) {
    absolute => 'Absolute',
    relative => 'Relative',
  };
}
