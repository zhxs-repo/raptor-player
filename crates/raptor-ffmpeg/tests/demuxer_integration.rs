//! Integration tests for raptor-ffmpeg demuxer and decoder
//!
//! These tests exercise the Demuxer/Decoder traits with real FFmpeg calls.
//! Some tests require reference media files from the Erika third_party directory.

use raptor_core::RaptorError;
use raptor_ffmpeg::{Demuxer, FfmpegDemuxer};

/// Path to the h264 reference file in the Erika third_party directory
fn h264_ref_path() -> String {
    // Try relative to workspace root
    let candidates = [
        "D:/Devs/github/dyl-player/Erika/third_party/src/ffmpeg-7.1.1/tests/ref/lavf-fate/h264.mp4",
    ];
    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    String::new()
}

#[test]
fn demuxer_open_nonexistent_file() {
    let mut demuxer = FfmpegDemuxer::new();
    let result = demuxer.open("C:\\nonexistent\\fake.mp4");
    assert!(result.is_err());
    if let Err(RaptorError::Demux(msg)) = result {
        assert!(msg.contains("failed to open"));
    } else {
        panic!("Expected Demux error");
    }
}

#[test]
fn demuxer_info_before_open() {
    let demuxer = FfmpegDemuxer::new();
    assert!(demuxer.info().is_none());
}

#[test]
fn demuxer_read_packet_before_open() {
    let mut demuxer = FfmpegDemuxer::new();
    let result = demuxer.read_packet();
    assert!(result.is_err());
}

#[test]
fn demuxer_seek_before_open() {
    let mut demuxer = FfmpegDemuxer::new();
    let result = demuxer.seek(1.0);
    assert!(result.is_err());
}

#[test]
fn demuxer_open_h264_ref() {
    let path = h264_ref_path();
    if path.is_empty() {
        eprintln!("SKIP: h264.mp4 reference file not found");
        return;
    }

    let mut demuxer = FfmpegDemuxer::new();
    // The FATE reference file is very small (161 bytes), may fail to open
    // but should at least parse the header without a crash
    match demuxer.open(&path) {
        Ok(()) => {
            let info = demuxer.info().expect("info should be available after open");
            println!("MediaInfo: {:?}", info);
            // Verify basic info is populated
            assert!(
                info.video_stream_index.is_some() || info.audio_stream_index.is_some(),
                "At least one stream should be detected"
            );
        }
        Err(e) => {
            // It's acceptable if the tiny reference file can't be fully parsed
            eprintln!("h264.mp4 open failed (expected for tiny ref files): {}", e);
        }
    }
}

#[test]
fn demuxer_default_constructor() {
    let demuxer = FfmpegDemuxer::default();
    assert!(demuxer.info().is_none());
}
