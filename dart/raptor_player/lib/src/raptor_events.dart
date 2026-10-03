/// Event types matching the Rust `RaptorEvent` enum (serde JSON).
///
/// Rust serde uses externally-tagged representation by default:
/// - Unit variants: `"End"`, `"PlaybackRestart"`
/// - Struct variants: `{"FileLoaded": {"duration": 120.0, ...}}`
library;

import 'dart:convert';

/// Base class for all Raptor events.
sealed class RaptorEvent {
  const RaptorEvent();

  /// Parse a JSON object (already decoded) into a [RaptorEvent].
  factory RaptorEvent.fromJson(Map<String, dynamic> json) {
    if (json.containsKey('FileLoaded')) {
      return FileLoadedEvent.fromJson(
        json['FileLoaded'] as Map<String, dynamic>,
      );
    }
    if (json.containsKey('EndFile')) {
      return EndFileEvent.fromJson(
        json['EndFile'] as Map<String, dynamic>,
      );
    }
    if (json.containsKey('Error')) {
      return ErrorEvent.fromJson(json['Error'] as Map<String, dynamic>);
    }
    if (json.containsKey('Seek')) {
      return SeekEvent.fromJson(json['Seek'] as Map<String, dynamic>);
    }
    if (json.containsKey('PlaybackRestart')) {
      return const PlaybackRestartEvent();
    }
    if (json.containsKey('End')) {
      return const EndEvent();
    }
    return UnknownEvent(json);
  }

  /// Parse from a raw JSON string.
  static RaptorEvent parse(String rawJson) {
    // Simple string variants (serde serializes unit variants as strings)
    if (rawJson == '"PlaybackRestart"') return const PlaybackRestartEvent();
    if (rawJson == '"End"') return const EndEvent();

    // Map variants
    try {
      final decoded = jsonDecode(rawJson);
      if (decoded is Map<String, dynamic>) {
        return RaptorEvent.fromJson(decoded);
      }
    } catch (_) {
      // Fall through
    }
    return UnknownEvent({'raw': rawJson});
  }
}

/// File loaded successfully.
class FileLoadedEvent extends RaptorEvent {
  const FileLoadedEvent({
    required this.duration,
    this.video,
    this.audio,
  });

  final double duration;
  final VideoInfo? video;
  final AudioInfo? audio;

  factory FileLoadedEvent.fromJson(Map<String, dynamic> json) {
    return FileLoadedEvent(
      duration: (json['duration'] as num).toDouble(),
      video: json['video'] != null
          ? VideoInfo.fromJson(json['video'] as Map<String, dynamic>)
          : null,
      audio: json['audio'] != null
          ? AudioInfo.fromJson(json['audio'] as Map<String, dynamic>)
          : null,
    );
  }

  @override
  String toString() =>
      'FileLoaded(duration=${duration.toStringAsFixed(2)}s, '
      'video=${video?.width}x${video?.height}, audio=${audio?.codec})';
}

/// File playback ended.
class EndFileEvent extends RaptorEvent {
  const EndFileEvent({required this.reason});

  final EndReason reason;

  factory EndFileEvent.fromJson(Map<String, dynamic> json) {
    return EndFileEvent(
      reason: EndReason.fromString(json['reason'] as String? ?? 'Eof'),
    );
  }

  @override
  String toString() => 'EndFile(reason=$reason)';
}

/// An error occurred.
class ErrorEvent extends RaptorEvent {
  const ErrorEvent({required this.code, required this.message});

  final int code;
  final String message;

  factory ErrorEvent.fromJson(Map<String, dynamic> json) {
    return ErrorEvent(
      code: json['code'] as int,
      message: json['message'] as String,
    );
  }

  @override
  String toString() => 'Error(code=$code, message=$message)';
}

/// Seek completed.
class SeekEvent extends RaptorEvent {
  const SeekEvent({required this.from, required this.to});

  final double from;
  final double to;

  factory SeekEvent.fromJson(Map<String, dynamic> json) {
    return SeekEvent(
      from: (json['from'] as num).toDouble(),
      to: (json['to'] as num).toDouble(),
    );
  }

  @override
  String toString() =>
      'Seek(from=${from.toStringAsFixed(2)}s, to=${to.toStringAsFixed(2)}s)';
}

/// Playback restarted (after resume).
class PlaybackRestartEvent extends RaptorEvent {
  const PlaybackRestartEvent();

  @override
  String toString() => 'PlaybackRestart';
}

/// Player terminated.
class EndEvent extends RaptorEvent {
  const EndEvent();

  @override
  String toString() => 'End';
}

/// Unknown or future event type.
class UnknownEvent extends RaptorEvent {
  const UnknownEvent(this.data);

  final Map<String, dynamic> data;

  @override
  String toString() => 'Unknown($data)';
}

// ═══════════════════════════════════════════════════
// Supporting types
// ═══════════════════════════════════════════════════

/// Video stream information.
class VideoInfo {
  const VideoInfo({
    required this.width,
    required this.height,
    required this.codec,
    required this.fps,
  });

  final int width;
  final int height;
  final String codec;
  final double fps;

  factory VideoInfo.fromJson(Map<String, dynamic> json) {
    return VideoInfo(
      width: json['width'] as int,
      height: json['height'] as int,
      codec: json['codec'] as String,
      fps: (json['fps'] as num).toDouble(),
    );
  }

  @override
  String toString() => '${width}x$height $codec ${fps.toStringAsFixed(2)}fps';
}

/// Audio stream information.
class AudioInfo {
  const AudioInfo({
    required this.codec,
    required this.channels,
    required this.sampleRate,
  });

  final String codec;
  final int channels;
  final int sampleRate;

  factory AudioInfo.fromJson(Map<String, dynamic> json) {
    return AudioInfo(
      codec: json['codec'] as String,
      channels: json['channels'] as int,
      sampleRate: json['sample_rate'] as int,
    );
  }

  @override
  String toString() => '$codec ${channels}ch ${sampleRate}Hz';
}

/// Reason for file playback ending.
enum EndReason {
  eof,
  error,
  stop;

  static EndReason fromString(String s) {
    return switch (s) {
      'Eof' => eof,
      'Error' => error,
      'Stop' => stop,
      _ => eof,
    };
  }
}
