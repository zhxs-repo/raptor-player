pub mod cpal_output;

pub use cpal_output::{AudioOutput, CpalOutput};

/// 应用音量（线性）到 f32 采样
pub fn apply_volume(samples: &mut [f32], volume: f32) {
    let vol = volume.clamp(0.0, 1.0);
    for s in samples.iter_mut() {
        *s *= vol;
    }
}
