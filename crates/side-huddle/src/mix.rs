    use crate::{AudioChunk, Recording};
        use crossbeam_channel::bounded;
        use std::thread;

        /// Mix two PCM-16 mono buffers of equal length.
        /// Samples are summed as i32 and clamped to i16 range.
        pub fn mix_pcm(a: &[i16], b: &[i16]) -> Vec<i16> {
            let len = a.len().min(b.len());
            let mut out = Vec::with_capacity(len);
            for i in 0..len {
                let sum = a[i] as i32 + b[i] as i32;
                out.push(sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
            }
            // Append any tail from the longer input as-is
            if a.len() > len { out.extend_from_slice(&a[len..]); }
            if b.len() > len { out.extend_from_slice(&b[len..]); }
            out
        }

        /// Combine two Recordings into one by mixing their PCM samples.
        /// Both inputs must use the same sample rate.
        /// The returned Recording ends when either input ends.
        pub fn mix_recordings(mut a: Recording, mut b: Recording, _sample_rate: u32) -> Recording {
            let (tx, rx) = bounded::<AudioChunk>(64);

            // Extract the stop callbacks (leaves None in a/b so their Drop is a no-op)
            let stop_a = a.stop_fn.take();
            let stop_b = b.stop_fn.take();
            // Clone the receivers — crossbeam Receiver is Clone; originals in a/b drop harmlessly
            let rx_a = a.rx.clone();
            let rx_b = b.rx.clone();
            drop(a);
            drop(b);

            thread::spawn(move || {
                // Drain both channels concurrently using crossbeam select
                let mut buf_a: Vec<i16> = Vec::new();
                let mut buf_b: Vec<i16> = Vec::new();

                loop {
                    // Accumulate from both sides
                    if let Ok(chunk) = rx_a.try_recv() {
                        buf_a.extend_from_slice(&chunk.pcm);
                    }
                    if let Ok(chunk) = rx_b.try_recv() {
                        buf_b.extend_from_slice(&chunk.pcm);
                    }

                    // Mix whatever we have in common
                    let mix_len = buf_a.len().min(buf_b.len());
                    if mix_len > 0 {
                        let mixed = mix_pcm(&buf_a[..mix_len], &buf_b[..mix_len]);
                        buf_a.drain(..mix_len);
                        buf_b.drain(..mix_len);
                        if tx.send(AudioChunk { pcm: mixed }).is_err() {
                            break;
                        }
                    } else {
                        // Nothing to mix yet — block until one side has data
                        crossbeam_channel::select! {
                            recv(rx_a) -> msg => match msg {
                                Ok(chunk) => buf_a.extend_from_slice(&chunk.pcm),
                                Err(_) => break, // a ended
                            },
                            recv(rx_b) -> msg => match msg {
                                Ok(chunk) => buf_b.extend_from_slice(&chunk.pcm),
                                Err(_) => break, // b ended
                            },
                        }
                    }
                }
            });

            Recording {
                rx,
                stop_fn: Some(Box::new(move || {
                    if let Some(f) = stop_a { f(); }
                    if let Some(f) = stop_b { f(); }
                })),
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn mix_sums_and_clamps() {
                let a = vec![20000i16, -20000, 0];
                let b = vec![20000i16, -20000, 1000];
                let out = mix_pcm(&a, &b);
                assert_eq!(out[0], 32767);   // clamped high
                assert_eq!(out[1], -32768);  // clamped low
                assert_eq!(out[2], 1000);    // no clamp needed
            }

            #[test]
            fn mix_handles_length_mismatch() {
                let a = vec![1i16, 2, 3, 4, 5];
                let b = vec![1i16, 2, 3];
                let out = mix_pcm(&a, &b);
                assert_eq!(out.len(), 5);
                assert_eq!(out[3], 4); // tail of 'a'
            }
        }
    