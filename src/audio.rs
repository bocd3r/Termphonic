use std::collections::VecDeque;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::Duration;

use rodio::{OutputStream, OutputStreamBuilder, Sink, Source};

pub(crate) struct PcmSource {
    receiver: Receiver<Vec<i16>>,
    buffer: VecDeque<i16>,
    channels: u16,
    sample_rate: u32,
    is_buffering: bool,
    prebuffer_size: usize,
    played_samples: u64,
    peak_sample: u32,
    shared_playback_level: Arc<AtomicU32>,
    shared_finished: Arc<AtomicBool>,
}

impl Iterator for PcmSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let mut disconnected = false;
        loop {
            match self.receiver.try_recv() {
                Ok(chunk) => self.buffer.extend(chunk),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if self.buffer.is_empty() && disconnected {
            self.shared_finished.store(true, Ordering::Release);
            return None;
        }

        if self.is_buffering {
            if self.buffer.len() >= self.prebuffer_size || disconnected {
                self.is_buffering = false;
            } else {
                return Some(0.0);
            }
        }

        match self.buffer.pop_front() {
            Some(sample) => {
                self.played_samples += 1;
                self.peak_sample = self.peak_sample.max(sample.unsigned_abs() as u32);
                if self.played_samples % 1024 == 0 {
                    self.shared_playback_level
                        .store(self.peak_sample, Ordering::Relaxed);
                    self.peak_sample = 0;
                }
                Some(sample as f32 / 32768.0)
            }
            None => {
                self.is_buffering = true;
                self.shared_playback_level.store(0, Ordering::Relaxed);
                Some(0.0)
            }
        }
    }
}

impl Source for PcmSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub(crate) struct AudioPlayer {
    sink: Sink,
    _stream: OutputStream,
    current_ffmpeg_child: Option<std::process::Child>,
    tx_samples: Option<Sender<Vec<i16>>>,
    current_url: Option<String>,
    playback_offset: Duration,
    playback_level: Arc<AtomicU32>,
    playback_finished: Arc<AtomicBool>,
}

impl AudioPlayer {
    pub(crate) fn new() -> Self {
        let stream = OutputStreamBuilder::open_default_stream()
            .expect("Failed to open default audio output stream");
        let sink = Sink::connect_new(&stream.mixer());
        sink.set_volume(0.5);

        let playback_level = Arc::new(AtomicU32::new(0));
        let playback_finished = Arc::new(AtomicBool::new(false));
        Self {
            sink,
            _stream: stream,
            current_ffmpeg_child: None,
            tx_samples: None,
            current_url: None,
            playback_offset: Duration::ZERO,
            playback_level,
            playback_finished,
        }
    }

    pub(crate) fn play(&mut self, url: &str, start_seconds: u64) {
        self.stop_current_process();

        let (tx, rx) = channel();
        self.tx_samples = Some(tx.clone());
        self.current_url = Some(url.to_string());
        self.playback_offset = Duration::from_secs(start_seconds);
        self.playback_level = Arc::new(AtomicU32::new(0));
        self.playback_finished = Arc::new(AtomicBool::new(false));

        let mut cmd = Command::new("ffmpeg");
        if start_seconds > 0 {
            cmd.arg("-ss").arg(start_seconds.to_string());
        }
        cmd.args([
            "-i",
            url,
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => return,
        };

        let mut stdout = child.stdout.take().expect("Failed to take stdout");
        self.current_ffmpeg_child = Some(child);

        std::thread::spawn(move || {
            let mut leftover = Vec::new();
            let mut buffer = [0u8; 16384];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut data = leftover;
                        data.extend_from_slice(&buffer[..n]);

                        let len = data.len();
                        let end = len - (len % 2);

                        if end > 0 {
                            let mut samples = Vec::with_capacity(end / 2);
                            for chunk in data[..end].chunks_exact(2) {
                                samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                            }
                            if tx.send(samples).is_err() {
                                break;
                            }
                        }

                        leftover = data[end..].to_vec();
                    }
                    Err(_) => break,
                }
            }
        });

        self.sink.stop();
        let source = PcmSource {
            receiver: rx,
            buffer: VecDeque::with_capacity(88_200),
            channels: 2,
            sample_rate: 44_100,
            is_buffering: true,
            prebuffer_size: 22_050,
            played_samples: 0,
            peak_sample: 0,
            shared_playback_level: Arc::clone(&self.playback_level),
            shared_finished: Arc::clone(&self.playback_finished),
        };
        self.sink.append(source);
        self.sink.play();
    }

    pub(crate) fn position(&self) -> Duration {
        self.playback_offset + self.sink.get_pos()
    }

    pub(crate) fn level(&self) -> f64 {
        self.playback_level.load(Ordering::Relaxed) as f64 / i16::MAX as f64
    }

    pub(crate) fn finished(&self) -> bool {
        self.playback_finished.load(Ordering::Acquire) || self.sink.empty()
    }

    pub(crate) fn seek(&mut self, seconds: u64) {
        if let Some(url) = self.current_url.clone() {
            self.play(&url, seconds);
        }
    }

    pub(crate) fn stop_current_process(&mut self) {
        if let Some(mut child) = self.current_ffmpeg_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.tx_samples = None;
    }

    pub(crate) fn pause(&self) {
        self.sink.pause();
    }

    pub(crate) fn resume(&self) {
        self.sink.play();
    }

    pub(crate) fn stop_sink(&self) {
        self.sink.stop();
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_pcm_source_as_finished_when_input_disconnects() {
        let (sender, receiver) = channel();
        drop(sender);
        let finished = Arc::new(AtomicBool::new(false));
        let mut source = PcmSource {
            receiver,
            buffer: VecDeque::new(),
            channels: 2,
            sample_rate: 44_100,
            is_buffering: true,
            prebuffer_size: 1,
            played_samples: 0,
            peak_sample: 0,
            shared_playback_level: Arc::new(AtomicU32::new(0)),
            shared_finished: Arc::clone(&finished),
        };

        assert_eq!(source.next(), None);
        assert!(finished.load(Ordering::Acquire));
    }
}
