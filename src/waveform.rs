use anyhow::{Context, Result, anyhow};
use std::f32::consts::PI;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SAMPLE_RATE: usize = 48_000;
const FRAME_SIZE: usize = 2_048;
const BAND_COUNT: usize = 64;
const MIN_FREQUENCY: f32 = 45.0;
const MAX_FREQUENCY: f32 = 16_000.0;
const DB_FLOOR: f32 = -72.0;
const DB_CEILING: f32 = -6.0;
const RETRY_DELAY: Duration = Duration::from_secs(1);

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

pub struct SpectrumService {
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    child_pid: Arc<AtomicU32>,
    snapshot: Arc<RwLock<SpectrumSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl SpectrumService {
    pub fn new() -> Self {
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let child_pid = Arc::new(AtomicU32::new(0));
        let snapshot = Arc::new(RwLock::new(SpectrumSnapshot::Idle));

        let worker_active = Arc::clone(&active);
        let worker_stop = Arc::clone(&stop);
        let worker_pid = Arc::clone(&child_pid);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                if !worker_active.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }

                set_snapshot(&worker_snapshot, SpectrumSnapshot::Starting);
                match capture_spectrum(&worker_active, &worker_stop, &worker_pid, &worker_snapshot)
                {
                    Ok(()) => {
                        if !worker_active.load(Ordering::Relaxed) {
                            set_snapshot(&worker_snapshot, SpectrumSnapshot::Idle);
                        }
                    }
                    Err(error) => {
                        if worker_active.load(Ordering::Relaxed)
                            && !worker_stop.load(Ordering::Relaxed)
                        {
                            set_snapshot(
                                &worker_snapshot,
                                SpectrumSnapshot::Error {
                                    message: Arc::from(error.to_string()),
                                },
                            );
                            sleep_interruptibly(&worker_active, &worker_stop, RETRY_DELAY);
                        }
                    }
                }
            }
        });

        Self {
            active,
            stop,
            child_pid,
            snapshot,
            worker: Some(worker),
        }
    }

    pub fn set_active(&self, active: bool) {
        let changed = self.active.swap(active, Ordering::Relaxed) != active;
        if changed && !active {
            set_snapshot(&self.snapshot, SpectrumSnapshot::Idle);
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
        self.active.store(false, Ordering::Relaxed);
        terminate_capture(self.child_pid.load(Ordering::Relaxed));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn capture_spectrum(
    active: &AtomicBool,
    stop: &AtomicBool,
    child_pid: &AtomicU32,
    snapshot: &RwLock<SpectrumSnapshot>,
) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (active, stop, child_pid, snapshot);
        return Err(anyhow!(
            "real-time spectrum capture currently requires PipeWire on Linux"
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let mut child = spawn_pipewire_capture()?;
        child_pid.store(child.id(), Ordering::Relaxed);
        let result = read_spectrum_stream(&mut child, active, stop, snapshot);
        let _ = child.kill();
        let _ = child.wait();
        child_pid.store(0, Ordering::Relaxed);
        result
    }
}

#[cfg(target_os = "linux")]
fn spawn_pipewire_capture() -> Result<Child> {
    Command::new("pw-cat")
        .args([
            "--record",
            "--raw",
            "--rate",
            "48000",
            "--channels",
            "1",
            "--channel-map",
            "mono",
            "--format",
            "f32",
            "--latency",
            "20ms",
            "--properties",
            r#"{"stream.capture.sink":true,"node.name":"ctlyrics-spectrum","media.role":"Music"}"#,
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(
            || "failed to start pw-cat; install PipeWire tools to enable the real-time spectrum",
        )
}

#[cfg(target_os = "linux")]
fn read_spectrum_stream(
    child: &mut Child,
    active: &AtomicBool,
    stop: &AtomicBool,
    snapshot: &RwLock<SpectrumSnapshot>,
) -> Result<()> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("pw-cat stdout is unavailable"))?;
    let mut analyzer = SpectrumAnalyzer::new();
    let mut samples = Vec::<f32>::with_capacity(FRAME_SIZE * 2);
    let mut pending_bytes = Vec::<u8>::new();
    let mut buffer = [0u8; 16 * 1024];

    while active.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
        let count = stdout.read(&mut buffer)?;
        if count == 0 {
            let status = child
                .try_wait()?
                .or_else(|| child.wait().ok())
                .ok_or_else(|| anyhow!("pw-cat stopped producing audio"))?;
            let mut error = String::new();
            if let Some(stderr) = child.stderr.as_mut() {
                let _ = stderr.read_to_string(&mut error);
            }
            let error = error.trim();
            return Err(if error.is_empty() {
                anyhow!("pw-cat exited with status {status}")
            } else {
                anyhow!("pw-cat exited with status {status}: {error}")
            });
        }

        pending_bytes.extend_from_slice(&buffer[..count]);
        let complete = pending_bytes.len() / 4 * 4;
        for bytes in pending_bytes[..complete].chunks_exact(4) {
            let sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if sample.is_finite() {
                samples.push(sample.clamp(-1.0, 1.0));
            }

            while samples.len() >= FRAME_SIZE {
                let frame = &samples[..FRAME_SIZE];
                let (levels, peaks) = analyzer.analyze(frame);
                set_snapshot(
                    snapshot,
                    SpectrumSnapshot::Ready {
                        levels: Arc::from(levels.into_boxed_slice()),
                        peaks: Arc::from(peaks.into_boxed_slice()),
                    },
                );
                samples.drain(..FRAME_SIZE);
            }
        }
        pending_bytes.drain(..complete);
    }

    Ok(())
}

struct SpectrumAnalyzer {
    window: Vec<f32>,
    coefficients: Vec<f32>,
    levels: Vec<f32>,
    peaks: Vec<f32>,
}

impl SpectrumAnalyzer {
    fn new() -> Self {
        let window = (0..FRAME_SIZE)
            .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f32 / (FRAME_SIZE - 1) as f32).cos())
            .collect::<Vec<_>>();

        let frequencies = (0..BAND_COUNT).map(band_frequency).collect::<Vec<_>>();

        let coefficients = frequencies
            .iter()
            .map(|frequency| {
                let omega = 2.0 * PI * *frequency / SAMPLE_RATE as f32;
                2.0 * omega.cos()
            })
            .collect::<Vec<_>>();

        Self {
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
            for level in &mut self.levels {
                *level *= 0.72;
            }
            for peak in &mut self.peaks {
                *peak = (*peak - 0.035).max(0.0);
            }
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
}

fn band_frequency(index: usize) -> f32 {
    let position = index as f32 / (BAND_COUNT - 1) as f32;
    MIN_FREQUENCY * (MAX_FREQUENCY / MIN_FREQUENCY).powf(position)
}

fn set_snapshot(snapshot: &RwLock<SpectrumSnapshot>, next: SpectrumSnapshot) {
    if let Ok(mut snapshot) = snapshot.write() {
        *snapshot = next;
    }
}

fn sleep_interruptibly(active: &AtomicBool, stop: &AtomicBool, duration: Duration) {
    let steps = (duration.as_millis() / 50).max(1);
    for _ in 0..steps {
        if !active.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn terminate_capture(pid: u32) {
    if pid == 0 {
        return;
    }

    #[cfg(target_os = "linux")]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_one_kilohertz_tone() {
        let mut analyzer = SpectrumAnalyzer::new();
        let samples = (0..FRAME_SIZE)
            .map(|index| {
                let time = index as f32 / SAMPLE_RATE as f32;
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
        let frequency = band_frequency(strongest);
        assert!((700.0..=1_350.0).contains(&frequency));
    }

    #[test]
    fn silence_decays_levels_and_peaks() {
        let mut analyzer = SpectrumAnalyzer::new();
        analyzer.levels.fill(0.8);
        analyzer.peaks.fill(0.9);

        let (levels, peaks) = analyzer.analyze(&vec![0.0; FRAME_SIZE]);
        assert!(levels.iter().all(|level| *level < 0.8));
        assert!(peaks.iter().all(|peak| *peak < 0.9));
    }

    #[test]
    fn frequencies_are_log_spaced_and_bounded() {
        assert!((band_frequency(0) - MIN_FREQUENCY).abs() < 0.1);
        assert!((band_frequency(BAND_COUNT - 1) - MAX_FREQUENCY).abs() < 1.0);
        assert!(band_frequency(1) / band_frequency(0) > 1.0);
    }
}
