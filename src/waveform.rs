use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, RwLock,
    mpsc::{self, Sender},
};
use std::thread::{self, JoinHandle};

const WAVEFORM_SAMPLE_RATE: usize = 8_000;
const WAVEFORM_BUCKET_HZ: usize = 50;
const WAVEFORM_BUCKET_SAMPLES: usize = WAVEFORM_SAMPLE_RATE / WAVEFORM_BUCKET_HZ;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WaveformPeak {
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Default)]
pub struct Waveform {
    pub peaks: Vec<WaveformPeak>,
}

impl Waveform {
    pub fn resample(&self, width: usize) -> Vec<WaveformPeak> {
        if width == 0 || self.peaks.is_empty() {
            return Vec::new();
        }

        let source_len = self.peaks.len();
        (0..width)
            .map(|column| {
                let start = (column * source_len / width).min(source_len - 1);
                let mut end = ((column + 1) * source_len / width).min(source_len);
                if end <= start {
                    end = (start + 1).min(source_len);
                }

                self.peaks[start..end].iter().copied().fold(
                    WaveformPeak {
                        min: 1.0,
                        max: -1.0,
                    },
                    |mut aggregate, peak| {
                        aggregate.min = aggregate.min.min(peak.min);
                        aggregate.max = aggregate.max.max(peak.max);
                        aggregate
                    },
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub enum WaveformSnapshot {
    #[default]
    Idle,
    Loading {
        file: PathBuf,
    },
    Ready {
        file: PathBuf,
        waveform: Arc<Waveform>,
    },
    Error {
        file: PathBuf,
        message: Arc<str>,
    },
}

impl WaveformSnapshot {
    pub fn file(&self) -> Option<&Path> {
        match self {
            Self::Idle => None,
            Self::Loading { file } | Self::Ready { file, .. } | Self::Error { file, .. } => {
                Some(file.as_path())
            }
        }
    }
}

pub struct WaveformService {
    sender: Option<Sender<PathBuf>>,
    requested: Arc<RwLock<Option<PathBuf>>>,
    snapshot: Arc<RwLock<WaveformSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl WaveformService {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<PathBuf>();
        let requested = Arc::new(RwLock::new(None::<PathBuf>));
        let snapshot = Arc::new(RwLock::new(WaveformSnapshot::Idle));
        let worker_requested = Arc::clone(&requested);
        let worker_snapshot = Arc::clone(&snapshot);

        let worker = thread::spawn(move || {
            let mut cache = HashMap::<PathBuf, Arc<Waveform>>::new();

            while let Ok(first) = receiver.recv() {
                let mut file = first;
                while let Ok(next) = receiver.try_recv() {
                    file = next;
                }

                let result = if let Some(cached) = cache.get(&file) {
                    Ok(Arc::clone(cached))
                } else {
                    decode_waveform(&file).map(|waveform| {
                        let waveform = Arc::new(waveform);
                        cache.insert(file.clone(), Arc::clone(&waveform));
                        waveform
                    })
                };

                let is_current = worker_requested
                    .read()
                    .ok()
                    .and_then(|requested| requested.clone())
                    .is_some_and(|requested| requested == file);
                if !is_current {
                    continue;
                }

                if let Ok(mut state) = worker_snapshot.write() {
                    *state = match result {
                        Ok(waveform) => WaveformSnapshot::Ready { file, waveform },
                        Err(error) => WaveformSnapshot::Error {
                            file,
                            message: Arc::from(error.to_string()),
                        },
                    };
                }
            }
        });

        Self {
            sender: Some(sender),
            requested,
            snapshot,
            worker: Some(worker),
        }
    }

    pub fn request(&self, file: &str) {
        let file = file.trim();
        if file.is_empty() {
            if let Ok(mut requested) = self.requested.write() {
                *requested = None;
            }
            if let Ok(mut snapshot) = self.snapshot.write() {
                *snapshot = WaveformSnapshot::Idle;
            }
            return;
        }

        let file = PathBuf::from(file);
        let unchanged = self
            .requested
            .read()
            .ok()
            .and_then(|requested| requested.clone())
            .is_some_and(|requested| requested == file);
        if unchanged {
            return;
        }

        if let Ok(mut requested) = self.requested.write() {
            *requested = Some(file.clone());
        }
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = WaveformSnapshot::Loading { file: file.clone() };
        }
        if let Some(sender) = &self.sender {
            let _ = sender.send(file);
        }
    }

    pub fn snapshot(&self) -> WaveformSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }
}

impl Default for WaveformService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WaveformService {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn decode_waveform(path: &Path) -> Result<Waveform> {
    if !path.is_file() {
        return Err(anyhow!("audio file does not exist: {}", path.display()));
    }

    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &WAVEFORM_SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to start ffmpeg; install ffmpeg to enable waveform view")?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stdout is unavailable"))?;
    let mut peaks = Vec::<WaveformPeak>::new();
    let mut bucket_min = 1.0_f32;
    let mut bucket_max = -1.0_f32;
    let mut samples_in_bucket = 0usize;
    let mut pending = Vec::<u8>::new();
    let mut buffer = [0u8; 16 * 1024];

    loop {
        let count = stdout.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..count]);

        let complete = pending.len() / 4 * 4;
        for bytes in pending[..complete].chunks_exact(4) {
            let sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if !sample.is_finite() {
                continue;
            }

            bucket_min = bucket_min.min(sample);
            bucket_max = bucket_max.max(sample);
            samples_in_bucket += 1;

            if samples_in_bucket >= WAVEFORM_BUCKET_SAMPLES {
                peaks.push(WaveformPeak {
                    min: bucket_min,
                    max: bucket_max,
                });
                bucket_min = 1.0;
                bucket_max = -1.0;
                samples_in_bucket = 0;
            }
        }
        pending.drain(..complete);
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(anyhow!(
            "ffmpeg failed to decode {}{}",
            path.display(),
            if error.is_empty() {
                String::new()
            } else {
                format!(": {error}")
            }
        ));
    }

    if samples_in_bucket > 0 {
        peaks.push(WaveformPeak {
            min: bucket_min,
            max: bucket_max,
        });
    }

    if peaks.is_empty() {
        return Err(anyhow!("audio stream produced no waveform samples"));
    }

    normalize_peaks(&mut peaks);
    Ok(Waveform { peaks })
}

fn normalize_peaks(peaks: &mut [WaveformPeak]) {
    let normalization = peaks
        .iter()
        .fold(0.0_f32, |maximum, peak| {
            maximum.max(peak.min.abs()).max(peak.max.abs())
        })
        .max(f32::EPSILON);

    for peak in peaks {
        peak.min = (peak.min / normalization).clamp(-1.0, 1.0);
        peak.max = (peak.max / normalization).clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_aggregates_extrema() {
        let waveform = Waveform {
            peaks: vec![
                WaveformPeak {
                    min: -0.2,
                    max: 0.4,
                },
                WaveformPeak {
                    min: -0.8,
                    max: 0.6,
                },
                WaveformPeak {
                    min: -0.1,
                    max: 0.2,
                },
                WaveformPeak {
                    min: -0.5,
                    max: 0.9,
                },
            ],
        };

        assert_eq!(
            waveform.resample(2),
            vec![
                WaveformPeak {
                    min: -0.8,
                    max: 0.6,
                },
                WaveformPeak {
                    min: -0.5,
                    max: 0.9,
                },
            ]
        );
    }

    #[test]
    fn resample_repeats_when_destination_is_wider() {
        let waveform = Waveform {
            peaks: vec![WaveformPeak {
                min: -0.5,
                max: 0.75,
            }],
        };

        assert_eq!(
            waveform.resample(3),
            vec![
                WaveformPeak {
                    min: -0.5,
                    max: 0.75,
                };
                3
            ]
        );
    }

    #[test]
    fn normalize_scales_peak_range() {
        let mut peaks = vec![
            WaveformPeak {
                min: -0.25,
                max: 0.5,
            },
            WaveformPeak {
                min: -1.0,
                max: 0.75,
            },
        ];
        normalize_peaks(&mut peaks);
        assert_eq!(peaks[1].min, -1.0);
        assert_eq!(peaks[0].max, 0.5);
    }
}
