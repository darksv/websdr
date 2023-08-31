use rustfft::num_complex::{Complex32, ComplexFloat};
use crate::iter::IterExt;

pub(crate) struct Demodulator {
    audio_samples: Vec<f32>,
}

impl Demodulator {
    pub(crate) fn new() -> Self {
        Self { audio_samples: vec![] }
    }
    pub(crate) fn push_samples(&mut self, samples: impl IntoIterator<Item=Complex32>) {
        let Fs = 1_200_000;
        let Fbw = 200_000;
        let Fa = 22050;

        samples
            .into_iter()
            .step_by(Fs / Fbw)
            .array_windows()
            .map(|[s0, s1]| (s1 * s0.conj()).arg())
            .step_by(Fbw / Fa)
            .collect_into(&mut self.audio_samples);
    }

    pub(crate) fn pull_audio(&mut self, n: usize) -> Option<impl Iterator<Item=f32> + '_> {
        if self.audio_samples.len() < n {
            return None;
        }
        Some(self.audio_samples.drain(..n))
    }
}
