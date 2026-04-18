/// Microphone capture using cpal (cross-platform audio I/O).
///
/// cpal wraps CoreAudio on macOS, WASAPI on Windows, ALSA/PipeWire on Linux.
/// Captures the default input device and converts F32 samples to PCM-16 mono.
use crossbeam_channel::bounded;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{AudioChunk, Error, Recording, Result};

pub(crate) fn start(sample_rate: u32, chunk_ms: u32) -> Result<Recording> {
    let host = cpal::default_host();

    let device = host
        .default_input_device()
        .ok_or_else(|| Error::RecordingFailed("no default input device found".into()))?;

    // Request PCM-16 mono at the target sample rate.
    // cpal uses SupportedStreamConfig — query what the device supports.
    let config = device
        .default_input_config()
        .map_err(|e| Error::RecordingFailed(format!("default_input_config: {e}")))?;

    // cpal::SampleRate is a type alias for u32 in cpal 0.17
    let native_rate    = config.sample_rate();
    let native_channels = config.channels() as u32;

    let frames_per_chunk = (sample_rate * chunk_ms / 1000) as usize;
    let (tx, rx) = bounded::<AudioChunk>(64);

    // Accumulate samples here; emit a chunk every `frames_per_chunk` mono frames.
    let mut buf: Vec<i16> = Vec::with_capacity(frames_per_chunk * 2);
    let target_rate = sample_rate;
    let tx2 = tx.clone();

    let stream = device
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                // Convert F32 → i16 and downmix to mono
                let mono_iter = data
                    .chunks(native_channels as usize)
                    .map(|frame| {
                        let sum: f32 = frame.iter().sum::<f32>() / native_channels as f32;
                        (sum.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                    });

                buf.extend(mono_iter);

                // Simple nearest-neighbour resampling if native rate ≠ target rate
                if native_rate != target_rate {
                    let ratio = native_rate as f64 / target_rate as f64;
                    let resampled: Vec<i16> = (0..((buf.len() as f64 / ratio) as usize))
                        .map(|i| {
                            let src = (i as f64 * ratio) as usize;
                            buf.get(src).copied().unwrap_or(0)
                        })
                        .collect();
                    buf.clear();
                    buf.extend(resampled);
                }

                // Emit complete chunks
                while buf.len() >= frames_per_chunk {
                    let chunk_samples: Vec<i16> = buf.drain(..frames_per_chunk).collect();
                    let chunk = AudioChunk { pcm: chunk_samples };
                    let _ = tx2.try_send(chunk); // drop if consumer is slow
                }
            },
            |err| { let _ = err; },
            None, // no timeout
        )
        .map_err(|e| Error::RecordingFailed(format!("build_input_stream: {e}")))?;

    stream
        .play()
        .map_err(|e| Error::RecordingFailed(format!("stream play: {e}")))?;

    // Keep stream alive; drop it to stop recording.
    let stop_fn: Box<dyn FnOnce() + Send> = Box::new(move || {
        drop(stream);
        drop(tx); // close sender so receiver drains
    });

    Ok(Recording { rx, stop_fn: Some(stop_fn) })
}
