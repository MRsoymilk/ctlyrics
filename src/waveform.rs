use anyhow::{Context, Result, anyhow};
use std::f32::consts::PI;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase};
use symphonia::default::{get_codecs, get_probe};

const FRAME_SIZE: usize = 2_048;
const BAND_COUNT: usize = 64;
const MIN_FREQUENCY: f32 = 45.0;
const MAX_FREQUENCY: f32 = 16_000.0;
const DB_FLOOR: f32 = -72.0;
const DB_CEILING: f32 = -6.0;
const IDLE_POLL: Duration = Duration::from_millis(50);
const ERROR_RETRY: Duration = Duration::from_secs(1);
const RESYNC_THRESHOLD_SECONDS: f64 = 0.45;

#[derive(Debug, Clone, Default)]
pub enum SpectrumSnapshot {
    #[default]
    Idle,
    Starting,
    Ready {
        levels: Arc<[f32]>,
        peaks: Arc<[f32]>,
    },
    Error {
        message: Arc<str>,
    },
}

#[derive(Debug, Clone)]
struct PlaybackState {
    active: bool,
    file: PathBuf,
    position: u64,
    status: String,
    anchor: Instant,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            active: false,
            file: PathBuf::new(),
            position: 0,
            status: "stopped".to_string(),
            anchor: Instant::now(),
        }
    }
}

impl PlaybackState {
    fn target_seconds(&self) -> f64 {
        let elapsed = if self.status == "playing" {
            self.anchor.elapsed().as_secs_f64()
        } else {
            0.0
        };
        self.position as f64 + elapsed
    }
}

pub struct SpectrumService {
    playback: Arc<RwLock<PlaybackState>>,
    stop: Arc<AtomicBool>,
    snapshot: Arc<RwLock<SpectrumSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl SpectrumService {
    pub fn new() -> Self {
        let playback = Arc::new(RwLock::new(PlaybackState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(RwLock::new(SpectrumSnapshot::Idle));

        let worker_playback = Arc::clone(&playback);
        let worker_stop = Arc::clone(&stop);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = thread::spawn(move || {
            run_worker(&worker_playback, &worker_stop, &worker_snapshot);
        });

        Self {
            playback,
            stop,
            snapshot,
            worker: Some(worker),
        }
    }

    pub fn update(&self, active: bool, file: &str, position: u64, status: &str) {
        let Ok(mut playback) = self.playback.write() else {
            return;
        };

        let file = PathBuf::from(file);
        let now = Instant::now();
        let state_changed = playback.active != active
            || playback.file != file
            || playback.status != status
            || playback.position != position;

        if state_changed {
            playback.active = active;
            playback.file = file;
            playback.position = position;
            playback.status.clear();
            playback.status.push_str(status);
            playback.anchor = now;
        }
    }

    pub fn snapshot(&self) -> SpectrumSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }
}

impl Default for SpectrumService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SpectrumService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    playback: &RwLock<PlaybackState>,
    stop: &AtomicBool,
    snapshot: &RwLock<SpectrumSnapshot>,
) {
    let mut source: Option<AudioSource> = None;
    let mut analyzer: Option<SpectrumAnalyzer> = None;
    let mut last_error_file = PathBuf::new();

    while !stop.load(Ordering::Relaxed) {
        let state = playback
            .read()
            .map(|state| state.clone())
            .unwrap_or_default();

        if !state.active {
            source = None;
            analyzer = None;
            set_snapshot(snapshot, SpectrumSnapshot::Idle);
            thread::sleep(IDLE_POLL);
            continue;
        }

        if state.file.as_os_str().is_empty() || state.status == "stopped" {
            source = None;
            decay_snapshot(snapshot, analyzer.as_mut());
            thread::sleep(Duration::from_millis(40));
            continue;
        }

        if state.status != "playing" {
            decay_snapshot(snapshot, analyzer.as_mut());
            thread::sleep(Duration::from_millis(40));
            continue;
        }

        let target = state.target_seconds();
        let source_needs_open = source
            .as_ref()
            .is_none_or(|source| source.path != state.file);

        if source_needs_open {
            set_snapshot(snapshot, SpectrumSnapshot::Starting);
            match AudioSource::open(&state.file, target) {
                Ok(opened) => {
                    analyzer = Some(SpectrumAnalyzer::new(opened.sample_rate as usize));
                    source = Some(opened);
                    last_error_file.clear();
                }
                Err(error) => {
                    if last_error_file != state.file {
                        tracing::warn!(path = %state.file.display(), %error, "spectrum decoder unavailable");
                        last_error_file = state.file.clone();
                    }
                    set_snapshot(
                        snapshot,
                        SpectrumSnapshot::Error {
                            message: Arc::from(error.to_string()),
                        },
                    );
                    sleep_interruptibly(stop, ERROR_RETRY);
                    continue;
                }
            }
        }

        let Some(source_ref) = source.as_mut() else {
            continue;
        };

        if (source_ref.position_seconds - target).abs() > RESYNC_THRESHOLD_SECONDS
            && let Err(error) = source_ref.seek(target)
        {
            set_snapshot(
                snapshot,
                SpectrumSnapshot::Error {
                    message: Arc::from(error.to_string()),
                },
            );
            source = None;
            sleep_interruptibly(stop, ERROR_RETRY);
            continue;
        }

        let started = Instant::now();
        match source_ref.read_frame(FRAME_SIZE) {
            Ok(Some(frame)) => {
                if analyzer
                    .as_ref()
                    .is_none_or(|analyzer| analyzer.sample_rate != frame.sample_rate as usize)
                {
                    analyzer = Some(SpectrumAnalyzer::new(frame.sample_rate as usize));
                }
                if let Some(analyzer) = analyzer.as_mut() {
                    let (levels, peaks) = analyzer.analyze(&frame.samples);
                    set_snapshot(
                        snapshot,
                        SpectrumSnapshot::Ready {
                            levels: Arc::from(levels.into_boxed_slice()),
                            peaks: Arc::from(peaks.into_boxed_slice()),
                        },
                    );
                }

                let frame_time =
                    Duration::from_secs_f64(frame.samples.len() as f64 / frame.sample_rate as f64);
                if let Some(remaining) = frame_time.checked_sub(started.elapsed()) {
                    sleep_interruptibly(stop, remaining);
                }
            }
            Ok(None) => {
                source = None;
                decay_snapshot(snapshot, analyzer.as_mut());
                thread::sleep(Duration::from_millis(40));
            }
            Err(error) => {
                set_snapshot(
                    snapshot,
                    SpectrumSnapshot::Error {
                        message: Arc::from(error.to_string()),
                    },
                );
                source = None;
                sleep_interruptibly(stop, ERROR_RETRY);
            }
        }
    }
}

struct AudioFrame {
    samples: Vec<f32>,
    sample_rate: u32,
}

struct AudioSource {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    time_base: Option<TimeBase>,
    pending: Vec<f32>,
    interleaved: Vec<f32>,
    position_seconds: f64,
}

impl AudioSource {
    fn open(path: &Path, position_seconds: f64) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("failed to open audio file {}", path.display()))?;
        let media_source = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            hint.with_extension(extension);
        }

        let format = get_probe()
            .probe(
                &hint,
                media_source,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| anyhow!("unsupported audio format: {error}"))?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("audio file contains no decodable audio track"))?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|params| params.audio())
            .ok_or_else(|| anyhow!("audio codec parameters are unavailable"))?
            .clone();
        let sample_rate = params
            .sample_rate
            .ok_or_else(|| anyhow!("audio sample rate is unavailable"))?;
        let time_base = track.time_base;
        let decoder = get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|error| anyhow!("unsupported audio codec: {error}"))?;

        let mut source = Self {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            sample_rate,
            time_base,
            pending: Vec::with_capacity(FRAME_SIZE * 4),
            interleaved: Vec::new(),
            position_seconds: 0.0,
        };
        source.seek(position_seconds)?;
        Ok(source)
    }

    fn seek(&mut self, position_seconds: f64) -> Result<()> {
        let target = position_seconds.max(0.0);
        let time = Time::try_from_secs_f64(target)
            .ok_or_else(|| anyhow!("invalid seek position: {target:.3}s"))?;
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| anyhow!("audio seek failed: {error}"))?;
        self.decoder.reset();
        self.pending.clear();
        self.position_seconds = self
            .time_base
            .and_then(|time_base| time_base.calc_time(seeked.actual_ts))
            .map(|time| time.as_secs_f64())
            .unwrap_or(target);
        Ok(())
    }

    fn read_frame(&mut self, frame_size: usize) -> Result<Option<AudioFrame>> {
        while self.pending.len() < frame_size {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(SymphoniaError::ResetRequired) => {
                    return Err(anyhow!("audio stream changed while decoding"));
                }
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(error) => return Err(anyhow!("failed to read audio packet: {error}")),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(error) => return Err(anyhow!("failed to decode audio packet: {error}")),
            };

            self.sample_rate = decoded.spec().rate();
            let channels = decoded.spec().channels().count().max(1);
            self.interleaved.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut self.interleaved);

            for frame in self.interleaved.chunks(channels) {
                let mono = frame.iter().copied().sum::<f32>() / channels as f32;
                self.pending.push(mono.clamp(-1.0, 1.0));
            }
        }

        if self.pending.is_empty() {
            return Ok(None);
        }

        let take = frame_size.min(self.pending.len());
        let samples = self.pending.drain(..take).collect::<Vec<_>>();
        self.position_seconds += samples.len() as f64 / self.sample_rate as f64;

        if samples.len() < frame_size {
            let mut padded = samples;
            padded.resize(frame_size, 0.0);
            return Ok(Some(AudioFrame {
                samples: padded,
                sample_rate: self.sample_rate,
            }));
        }

        Ok(Some(AudioFrame {
            samples,
            sample_rate: self.sample_rate,
        }))
    }
}

struct SpectrumAnalyzer {
    sample_rate: usize,
    window: Vec<f32>,
    coefficients: Vec<f32>,
    levels: Vec<f32>,
    peaks: Vec<f32>,
}

impl SpectrumAnalyzer {
    fn new(sample_rate: usize) -> Self {
        let sample_rate = sample_rate.max(1);
        let window = (0..FRAME_SIZE)
            .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f32 / (FRAME_SIZE - 1) as f32).cos())
            .collect::<Vec<_>>();
        let nyquist = sample_rate as f32 * 0.5;
        let max_frequency = MAX_FREQUENCY.min(nyquist * 0.92).max(MIN_FREQUENCY);
        let coefficients = (0..BAND_COUNT)
            .map(|index| {
                let frequency = band_frequency(index, max_frequency);
                let omega = 2.0 * PI * frequency / sample_rate as f32;
                2.0 * omega.cos()
            })
            .collect::<Vec<_>>();

        Self {
            sample_rate,
            window,
            coefficients,
            levels: vec![0.0; BAND_COUNT],
            peaks: vec![0.0; BAND_COUNT],
        }
    }

    fn analyze(&mut self, samples: &[f32]) -> (Vec<f32>, Vec<f32>) {
        debug_assert_eq!(samples.len(), FRAME_SIZE);

        let rms = (samples.iter().map(|sample| sample * sample).sum::<f32>()
            / samples.len() as f32)
            .sqrt();
        if rms < 0.00008 {
            self.decay();
            return (self.levels.clone(), self.peaks.clone());
        }

        for (band, coefficient) in self.coefficients.iter().copied().enumerate() {
            let mut previous = 0.0_f32;
            let mut previous2 = 0.0_f32;

            for (sample, window) in samples.iter().zip(&self.window) {
                let current = sample * window + coefficient * previous - previous2;
                previous2 = previous;
                previous = current;
            }

            let power =
                previous2 * previous2 + previous * previous - coefficient * previous * previous2;
            let magnitude = power.max(0.0).sqrt() / (FRAME_SIZE as f32 * 0.5);
            let db = 20.0 * magnitude.max(1.0e-9).log10();
            let raw_level = ((db - DB_FLOOR) / (DB_CEILING - DB_FLOOR)).clamp(0.0, 1.0);
            let shaped = raw_level.powf(0.72);

            let smoothing = if shaped > self.levels[band] {
                0.62
            } else {
                0.16
            };
            self.levels[band] += (shaped - self.levels[band]) * smoothing;

            if self.levels[band] >= self.peaks[band] {
                self.peaks[band] = self.levels[band];
            } else {
                self.peaks[band] = (self.peaks[band] - 0.018).max(self.levels[band]);
            }
        }

        (self.levels.clone(), self.peaks.clone())
    }

    fn decay(&mut self) {
        for level in &mut self.levels {
            *level *= 0.72;
        }
        for peak in &mut self.peaks {
            *peak = (*peak - 0.035).max(0.0);
        }
    }

    fn current(&self) -> (Vec<f32>, Vec<f32>) {
        (self.levels.clone(), self.peaks.clone())
    }
}

fn decay_snapshot(snapshot: &RwLock<SpectrumSnapshot>, analyzer: Option<&mut SpectrumAnalyzer>) {
    if let Some(analyzer) = analyzer {
        analyzer.decay();
        let (levels, peaks) = analyzer.current();
        set_snapshot(
            snapshot,
            SpectrumSnapshot::Ready {
                levels: Arc::from(levels.into_boxed_slice()),
                peaks: Arc::from(peaks.into_boxed_slice()),
            },
        );
    } else {
        set_snapshot(snapshot, SpectrumSnapshot::Idle);
    }
}

fn band_frequency(index: usize, max_frequency: f32) -> f32 {
    let position = index as f32 / (BAND_COUNT - 1) as f32;
    MIN_FREQUENCY * (max_frequency / MIN_FREQUENCY).powf(position)
}

fn set_snapshot(snapshot: &RwLock<SpectrumSnapshot>, next: SpectrumSnapshot) {
    if let Ok(mut snapshot) = snapshot.write() {
        *snapshot = next;
    }
}

fn sleep_interruptibly(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_one_kilohertz_tone() {
        let mut analyzer = SpectrumAnalyzer::new(48_000);
        let samples = (0..FRAME_SIZE)
            .map(|index| {
                let time = index as f32 / 48_000.0;
                (2.0 * PI * 1_000.0 * time).sin() * 0.7
            })
            .collect::<Vec<_>>();

        let (levels, _) = analyzer.analyze(&samples);
        let strongest = levels
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
            .unwrap();
        let frequency = band_frequency(strongest, MAX_FREQUENCY);
        assert!((700.0..=1_350.0).contains(&frequency));
    }

    #[test]
    fn silence_decays_levels_and_peaks() {
        let mut analyzer = SpectrumAnalyzer::new(44_100);
        analyzer.levels.fill(0.8);
        analyzer.peaks.fill(0.9);

        let (levels, peaks) = analyzer.analyze(&vec![0.0; FRAME_SIZE]);
        assert!(levels.iter().all(|level| *level < 0.8));
        assert!(peaks.iter().all(|peak| *peak < 0.9));
    }

    #[test]
    fn frequencies_are_log_spaced_and_bounded() {
        assert!((band_frequency(0, MAX_FREQUENCY) - MIN_FREQUENCY).abs() < 0.1);
        assert!((band_frequency(BAND_COUNT - 1, MAX_FREQUENCY) - MAX_FREQUENCY).abs() < 1.0);
        assert!(band_frequency(1, MAX_FREQUENCY) / band_frequency(0, MAX_FREQUENCY) > 1.0);
    }

    #[test]
    fn playback_target_advances_while_playing() {
        let state = PlaybackState {
            active: true,
            file: PathBuf::from("song.flac"),
            position: 10,
            status: "playing".to_string(),
            anchor: Instant::now() - Duration::from_millis(250),
        };
        assert!((10.20..10.40).contains(&state.target_seconds()));
    }
}
