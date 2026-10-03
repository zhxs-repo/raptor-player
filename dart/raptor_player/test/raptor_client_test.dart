import 'dart:convert';

import 'package:raptor_player/src/raptor_bindings.dart';
import 'package:raptor_player/src/raptor_client.dart';
import 'package:raptor_player/src/raptor_commands.dart';
import 'package:raptor_player/src/raptor_events.dart';
import 'package:test/test.dart';

void main() {
  group('RaptorCommands', () {
    test('loadFile produces correct JSON', () {
      final json = RaptorCommands.loadFile('/path/to/video.mp4');
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      expect(decoded.containsKey('LoadFile'), isTrue);
      final inner = decoded['LoadFile'] as Map<String, dynamic>;
      expect(inner['url'], '/path/to/video.mp4');
    });

    test('play is a string variant', () {
      expect(RaptorCommands.play, '"Play"');
    });

    test('pause is a string variant', () {
      expect(RaptorCommands.pause, '"Pause"');
    });

    test('togglePause is a string variant', () {
      expect(RaptorCommands.togglePause, '"TogglePause"');
    });

    test('stop is a string variant', () {
      expect(RaptorCommands.stop, '"Stop"');
    });

    test('seek with Absolute mode', () {
      final json = RaptorCommands.seek(5.5);
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['Seek'] as Map<String, dynamic>;
      expect(inner['target'], 5.5);
      expect(inner['mode'], 'Absolute');
    });

    test('seek with Relative mode', () {
      final json = RaptorCommands.seek(-2.0, mode: SeekMode.relative);
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['Seek'] as Map<String, dynamic>;
      expect(inner['target'], -2.0);
      expect(inner['mode'], 'Relative');
    });

    test('setVolume produces correct JSON', () {
      final json = RaptorCommands.setVolume(80);
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['SetVolume'] as Map<String, dynamic>;
      expect(inner['volume'], 80);
    });

    test('loadSubtitle produces correct JSON', () {
      final json = RaptorCommands.loadSubtitle('/path/to/sub.srt');
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['LoadSubtitle'] as Map<String, dynamic>;
      expect(inner['path'], '/path/to/sub.srt');
    });

    test('toggleSubtitle is a string variant', () {
      expect(RaptorCommands.toggleSubtitle, '"ToggleSubtitle"');
    });

    test('loadDanmaku produces correct JSON', () {
      final json = RaptorCommands.loadDanmaku('/path/to/danmaku.xml');
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['LoadDanmaku'] as Map<String, dynamic>;
      expect(inner['path'], '/path/to/danmaku.xml');
    });

    test('toggleDanmaku is a string variant', () {
      expect(RaptorCommands.toggleDanmaku, '"ToggleDanmaku"');
    });

    test('setDanmakuOpacity produces correct JSON', () {
      final json = RaptorCommands.setDanmakuOpacity(50);
      final decoded = jsonDecode(json) as Map<String, dynamic>;
      final inner = decoded['SetDanmakuOpacity'] as Map<String, dynamic>;
      expect(inner['opacity'], 50);
    });

    test('quit is a string variant', () {
      expect(RaptorCommands.quit, '"Quit"');
    });
  });

  group('RaptorEvent.parse', () {
    test('parses PlaybackRestart string variant', () {
      final event = RaptorEvent.parse('"PlaybackRestart"');
      expect(event, isA<PlaybackRestartEvent>());
    });

    test('parses End string variant', () {
      final event = RaptorEvent.parse('"End"');
      expect(event, isA<EndEvent>());
    });

    test('parses FileLoaded map variant', () {
      final json = jsonEncode({
        'FileLoaded': {
          'duration': 120.5,
          'video': {
            'width': 1920,
            'height': 1080,
            'codec': 'H264',
            'fps': 23.976,
          },
          'audio': {
            'codec': 'AAC',
            'channels': 2,
            'sample_rate': 48000,
          },
        },
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<FileLoadedEvent>());
      final fl = event as FileLoadedEvent;
      expect(fl.duration, 120.5);
      expect(fl.video!.width, 1920);
      expect(fl.video!.height, 1080);
      expect(fl.video!.codec, 'H264');
      expect(fl.video!.fps, closeTo(23.976, 0.001));
      expect(fl.audio!.codec, 'AAC');
      expect(fl.audio!.channels, 2);
      expect(fl.audio!.sampleRate, 48000);
    });

    test('parses FileLoaded without video/audio', () {
      final json = jsonEncode({
        'FileLoaded': {'duration': 60.0, 'video': null, 'audio': null},
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<FileLoadedEvent>());
      final fl = event as FileLoadedEvent;
      expect(fl.duration, 60.0);
      expect(fl.video, isNull);
      expect(fl.audio, isNull);
    });

    test('parses Seek map variant', () {
      final json = jsonEncode({
        'Seek': {'from': 1.0, 'to': 5.5},
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<SeekEvent>());
      final s = event as SeekEvent;
      expect(s.from, 1.0);
      expect(s.to, 5.5);
    });

    test('parses Error map variant', () {
      final json = jsonEncode({
        'Error': {'code': -3, 'message': 'file not found'},
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<ErrorEvent>());
      final e = event as ErrorEvent;
      expect(e.code, -3);
      expect(e.message, 'file not found');
    });

    test('parses EndFile map variant', () {
      final json = jsonEncode({
        'EndFile': {'reason': 'Eof'},
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<EndFileEvent>());
      final ef = event as EndFileEvent;
      expect(ef.reason, EndReason.eof);
    });

    test('parses EndFile with Stop reason', () {
      final json = jsonEncode({
        'EndFile': {'reason': 'Stop'},
      });
      final event = RaptorEvent.parse(json);
      expect(event, isA<EndFileEvent>());
      expect((event as EndFileEvent).reason, EndReason.stop);
    });

    test('returns UnknownEvent for unrecognized JSON', () {
      final event = RaptorEvent.parse('{"FutureVariant": {}}');
      expect(event, isA<UnknownEvent>());
    });

    test('returns UnknownEvent for invalid JSON', () {
      final event = RaptorEvent.parse('not json at all');
      expect(event, isA<UnknownEvent>());
    });
  });

  group('RaptorErrorCode', () {
    test('name returns correct names', () {
      expect(RaptorErrorCode.name(0), 'Ok');
      expect(RaptorErrorCode.name(-1), 'InvalidArgument');
      expect(RaptorErrorCode.name(-2), 'InvalidState');
      expect(RaptorErrorCode.name(-3), 'FileNotFound');
      expect(RaptorErrorCode.name(-99), 'Internal');
    });

    test('name returns Unknown for unrecognized codes', () {
      expect(RaptorErrorCode.name(-42), 'Unknown(-42)');
    });
  });

  group('Supporting types', () {
    test('VideoInfo toString', () {
      const info = VideoInfo(
        width: 1920,
        height: 1080,
        codec: 'H264',
        fps: 24.0,
      );
      expect(info.toString(), contains('1920x1080'));
      expect(info.toString(), contains('H264'));
    });

    test('AudioInfo toString', () {
      const info = AudioInfo(
        codec: 'AAC',
        channels: 2,
        sampleRate: 48000,
      );
      expect(info.toString(), contains('AAC'));
      expect(info.toString(), contains('2ch'));
      expect(info.toString(), contains('48000Hz'));
    });

    test('EndReason.fromString handles all values', () {
      expect(EndReason.fromString('Eof'), EndReason.eof);
      expect(EndReason.fromString('Error'), EndReason.error);
      expect(EndReason.fromString('Stop'), EndReason.stop);
      expect(EndReason.fromString('Unknown'), EndReason.eof); // fallback
    });

    test('SeekMode names match Rust serde', () {
      expect(SeekMode.absolute.name, 'Absolute');
      expect(SeekMode.relative.name, 'Relative');
    });
  });

  group('Event toString', () {
    test('FileLoadedEvent toString', () {
      const event = FileLoadedEvent(
        duration: 120.5,
        video: VideoInfo(
          width: 1920,
          height: 1080,
          codec: 'H264',
          fps: 24.0,
        ),
        audio: AudioInfo(
          codec: 'AAC',
          channels: 2,
          sampleRate: 48000,
        ),
      );
      final str = event.toString();
      expect(str, contains('120.50s'));
      expect(str, contains('1920x1080'));
    });

    test('SeekEvent toString', () {
      const event = SeekEvent(from: 1.0, to: 5.5);
      final str = event.toString();
      expect(str, contains('1.00s'));
      expect(str, contains('5.50s'));
    });

    test('ErrorEvent toString', () {
      const event = ErrorEvent(code: -3, message: 'file not found');
      expect(event.toString(), contains('-3'));
      expect(event.toString(), contains('file not found'));
    });

    test('PlaybackRestartEvent toString', () {
      expect(const PlaybackRestartEvent().toString(), 'PlaybackRestart');
    });

    test('EndEvent toString', () {
      expect(const EndEvent().toString(), 'End');
    });
  });

  group('RaptorException', () {
    test('toString includes error name and message', () {
      final ex = RaptorException(RaptorErrorCode.fileNotFound, 'no file');
      expect(ex.toString(), contains('FileNotFound'));
      expect(ex.toString(), contains('no file'));
    });
  });

}
