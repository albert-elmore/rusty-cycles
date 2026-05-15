//! Sample-accurate playback: one cpal output stream mixes voices on the audio clock.
//! Hit times are expressed in **output frames** (stereo pair = one frame) so closely spaced
//! triggers stay phase-locked relative to each other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimiterMode {
    None,
    Soft,
}

/// One decode/mix enqueue for [`Audio::schedule_voices`] (batch scheduling).
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ScheduledVoice {
    pub start_frame: u64,
    pub path: PathBuf,
    pub gain: f32,
    pub pan: f32,
    pub max_duration_ms: Option<f32>,
    pub cut_group: Option<i32>,
    pub cut_voices: Option<u32>,
    pub cutoff_ms: Option<f32>,
}

/// Decoded clip + mix params; frame offsets are applied in
/// [`Audio::commit_prepared_voices_at`] relative to an explicit cycle base.
#[derive(Clone, Debug)]
pub struct PreparedVoice {
    pub offset_frames: u64,
    pub data: Arc<Vec<f32>>,
    pub frame_count: usize,
    pub gain: f32,
    pub pan: f32,
    pub cut_group: Option<i32>,
    pub cut_voices: Option<u32>,
    pub cutoff_ms: Option<f32>,
}

fn validate_voice_schedule(
    gain: f32,
    pan: f32,
    max_duration_ms: Option<f32>,
    cut_group: Option<i32>,
    cut_voices: Option<u32>,
    cutoff_ms: Option<f32>,
) -> Result<(), String> {
    if !gain.is_finite() || gain < 0.0 {
        return Err(format!("invalid gain: {gain}"));
    }
    if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
        return Err(format!("invalid pan: {pan} (expected -1..=1)"));
    }
    if let Some(ms) = max_duration_ms {
        if !ms.is_finite() || ms < 0.0 {
            return Err(format!("invalid duration ms: {ms}"));
        }
    }
    if let Some(g) = cut_group {
        if g < 0 {
            return Err(format!("invalid cut group: {g}"));
        }
    }
    if let Some(v) = cut_voices {
        if v < 1 {
            return Err("invalid cut voices: must be >= 1".into());
        }
    }
    if let Some(ms) = cutoff_ms {
        if !ms.is_finite() || ms < 0.0 {
            return Err(format!("invalid cutoff ms: {ms}"));
        }
    }
    Ok(())
}

fn push_prepared_voice(
    voices: &mut Vec<Voice>,
    sample_rate: u32,
    start_frame: u64,
    data: Arc<Vec<f32>>,
    frame_count: usize,
    gain: f32,
    pan: f32,
    cut_group: Option<i32>,
    cut_voices: Option<u32>,
    cutoff_ms: Option<f32>,
) {
    if let Some(group) = cut_group {
        let voices_max = cut_voices.unwrap_or(1).max(1) as usize;
        let cutoff_frames = ((cutoff_ms.unwrap_or(0.0) as f64 / 1000.0) * sample_rate as f64)
            .round()
            .max(0.0) as u64;
        let mut idxs: Vec<usize> = voices
            .iter()
            .enumerate()
            .filter_map(|(i, v)| if v.cut_group == Some(group) { Some(i) } else { None })
            .collect();
        idxs.sort_by_key(|&i| voices[i].start);
        while idxs.len() + 1 > voices_max {
            let victim = idxs.remove(0);
            if let Some(v) = voices.get_mut(victim) {
                let end = start_frame.saturating_add(cutoff_frames);
                let keep = end.saturating_sub(v.start) as usize;
                if keep < v.frame_count {
                    v.frame_count = keep.max(1);
                    v.fade_out_frames =
                        (((cutoff_frames as usize).max(1)).min(v.frame_count)).max(1);
                }
            }
        }
    }
    let fade_out_frames =
        ((sample_rate as f64 * 0.004).round() as usize).clamp(0, frame_count);
    voices.push(Voice {
        start: start_frame,
        gain,
        pan,
        cut_group,
        fade_out_frames,
        data,
        frame_count,
    });
}

/// One voice: stereo interleaved f32 at device rate, starting at global frame `start`.
struct Voice {
    start: u64,
    gain: f32,
    /// -1 = left, 0 = center, 1 = right.
    pan: f32,
    cut_group: Option<i32>,
    fade_out_frames: usize,
    data: Arc<Vec<f32>>,
    /// Number of **frames** (not samples); `data.len() == frame_count * 2` for stereo.
    frame_count: usize,
}

pub struct Audio {
    _stream: Stream,
    sample_rate: u32,
    out_channels: usize,
    /// Total output **frames** elapsed (stereo frame = one tick).
    played_frames: Arc<AtomicU64>,
    /// Output frames where the **master** mix exceeded ±1.0 on any channel (see `take_clip_frames`).
    clipped_output_frames: Arc<AtomicU64>,
    /// Peak absolute sample seen on master bus since last poll.
    peak_abs_since_poll: Arc<std::sync::atomic::AtomicU32>,
    limiter_mode: LimiterMode,
    voices: Arc<RwLock<Vec<Voice>>>,
    cache: Arc<RwLock<HashMap<PathBuf, Arc<Vec<f32>>>>>,
}

impl Audio {
    pub fn try_new_with_limiter(limiter_mode: LimiterMode) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default audio output device".to_string())?;
        let default = device.default_output_config().map_err(|e| e.to_string())?;

        if default.sample_format() != SampleFormat::F32 {
            return Err(format!(
                "need f32 output (got {:?}); pick a device that supports f32",
                default.sample_format()
            ));
        }

        let config: StreamConfig = default.into();
        let sample_rate = config.sample_rate.0;
        let out_channels = config.channels as usize;
        if out_channels == 0 || out_channels > 2 {
            return Err(format!(
                "need 1 or 2 output channels (got {})",
                out_channels
            ));
        }

        let played_frames = Arc::new(AtomicU64::new(0));
        let clipped_output_frames = Arc::new(AtomicU64::new(0));
        let peak_abs_since_poll = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let voices = Arc::new(RwLock::new(Vec::<Voice>::new()));
        let played_cb = played_frames.clone();
        let voices_cb = voices.clone();
        let clip_cb = clipped_output_frames.clone();
        let peak_cb = peak_abs_since_poll.clone();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let frames = data.len() / out_channels;
                    let g0 = played_cb.load(Ordering::Acquire);
                    let vlist = match voices_cb.read() {
                        Ok(v) => v,
                        Err(_) => return,
                    };

                    data.fill(0.0);
                    let mut clipped_here = 0u64;

                    for f in 0..frames {
                        let g = g0 + f as u64;
                        let mut l = 0.0f32;
                        let mut r = 0.0f32;
                        for v in vlist.iter() {
                            if g < v.start {
                                continue;
                            }
                            let rel = (g - v.start) as usize;
                            if rel >= v.frame_count {
                                continue;
                            }
                            let i = rel * 2;
                            if i + 1 < v.data.len() {
                                let pan = v.pan.clamp(-1.0, 1.0);
                                // Linear pan: left=(1-pan)/2, right=(1+pan)/2, x2 to preserve center loudness.
                                let l_mul = 1.0 - ((pan + 1.0) * 0.5);
                                let r_mul = (pan + 1.0) * 0.5;
                                let mut tail_mul = 1.0f32;
                                if v.fade_out_frames > 0 && rel + v.fade_out_frames >= v.frame_count {
                                    let rem = v.frame_count.saturating_sub(rel);
                                    tail_mul = (rem as f32 / v.fade_out_frames as f32).clamp(0.0, 1.0);
                                }
                                l += v.data[i] * v.gain * (l_mul * 2.0) * tail_mul;
                                r += v.data[i + 1] * v.gain * (r_mul * 2.0) * tail_mul;
                            }
                        }
                        let base = f * out_channels;
                        if out_channels == 2 {
                            let (l_out, r_out) = match limiter_mode {
                                LimiterMode::None => (l, r),
                                LimiterMode::Soft => (soft_clip(l), soft_clip(r)),
                            };
                            data[base] = l_out;
                            data[base + 1] = r_out;
                            if l_out.abs() > 1.0 || r_out.abs() > 1.0 {
                                clipped_here += 1;
                            }
                        } else {
                            let m = (l + r) * 0.5;
                            let m_out = match limiter_mode {
                                LimiterMode::None => m,
                                LimiterMode::Soft => soft_clip(m),
                            };
                            data[base] = m_out;
                            if m_out.abs() > 1.0 {
                                clipped_here += 1;
                            }
                        }
                    }

                    if clipped_here > 0 {
                        clip_cb.fetch_add(clipped_here, Ordering::Relaxed);
                    }
                    let mut peak = 0.0f32;
                    for &x in data.iter() {
                        let a = x.abs();
                        if a > peak {
                            peak = a;
                        }
                    }
                    let peak_bits = peak.to_bits();
                    let mut prev = peak_cb.load(Ordering::Relaxed);
                    while f32::from_bits(prev) < peak {
                        match peak_cb.compare_exchange_weak(
                            prev,
                            peak_bits,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(actual) => prev = actual,
                        }
                    }

                    let g_end = g0 + frames as u64;
                    played_cb.store(g_end, Ordering::Release);
                    drop(vlist);

                    if let Ok(mut w) = voices_cb.write() {
                        w.retain(|v| v.start + v.frame_count as u64 > g_end);
                    }
                },
                |e| eprintln!("[cpal] {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;

        Ok(Audio {
            _stream: stream,
            sample_rate,
            out_channels,
            played_frames,
            clipped_output_frames,
            peak_abs_since_poll,
            limiter_mode,
            voices,
            cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Read and reset the count of output frames where |master| exceeded 1.0 (per channel for stereo).
    pub fn take_clip_frames(&self) -> u64 {
        self.clipped_output_frames.swap(0, Ordering::Relaxed)
    }

    /// Read and reset absolute master-peak sample since last call.
    pub fn take_peak_abs(&self) -> f32 {
        f32::from_bits(self.peak_abs_since_poll.swap(0, Ordering::Relaxed))
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    pub fn limiter_mode(&self) -> LimiterMode {
        self.limiter_mode
    }

    /// Frames per musical cycle at this BPM (integer math; ≥ 1).
    pub fn cycle_len_frames(&self, bpm: u32) -> u64 {
        let b = bpm.max(1) as u64;
        (self.sample_rate as u64 * 60 / b).max(1)
    }

    pub fn played_frames(&self) -> u64 {
        self.played_frames.load(Ordering::Relaxed)
    }

    /// Next cycle boundary at or after current playback (in frames).
    pub fn align_next_cycle_base(&self, cycle_len: u64) -> u64 {
        let s = self.played_frames.load(Ordering::Relaxed);
        if cycle_len <= 1 {
            return s;
        }
        if s == 0 {
            return 0;
        }
        s.div_ceil(cycle_len) * cycle_len
    }

    /// Block until `played_frames >= target` (hybrid sleep + spin).
    pub fn wait_until_frame(&self, target: u64) {
        loop {
            let now = self.played_frames.load(Ordering::Acquire);
            if now >= target {
                return;
            }
            let remain = target - now;
            let ms = (remain as f64 * 1000.0 / self.sample_rate as f64).max(0.0);
            if ms > 2.0 {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64 - 1));
            } else {
                std::hint::spin_loop();
            }
        }
    }

    pub fn clear_cache(&self) {
        if let Ok(mut c) = self.cache.write() {
            c.clear();
        }
    }

    #[allow(dead_code)]
    pub fn stop_playback(&self) {
        if let Ok(mut v) = self.voices.write() {
            v.clear();
        }
    }

    /// Decode `path` into the device-rate cache so [`schedule_voice`] / [`schedule_voices`] do not
    /// block on disk/resample on the cycle-critical path.
    #[allow(dead_code)]
    pub fn preload_clip(&self, path: &Path) -> Result<(), String> {
        self.clip_for_device(path)?;
        Ok(())
    }

    /// `Ok(None)` means skip (trimmed to zero); `Err` is load/decode failure.
    fn load_clip_frames(
        &self,
        path: &Path,
        max_duration_ms: Option<f32>,
    ) -> Result<Option<(Arc<Vec<f32>>, usize)>, String> {
        let data = self.clip_for_device(path)?;
        let mut frame_count = data.len() / 2;
        if let Some(ms) = max_duration_ms {
            let dur_frames = ((ms as f64 / 1000.0) * self.sample_rate as f64)
                .round()
                .max(0.0) as usize;
            frame_count = frame_count.min(dur_frames);
        }
        if frame_count == 0 {
            return Ok(None);
        }
        Ok(Some((data, frame_count)))
    }

    /// Validate and decode before the cycle wait; pairs with [`Audio::commit_prepared_voices_at`].
    pub fn prepare_voice_for_commit(
        &self,
        path: &Path,
        gain: f32,
        pan: f32,
        max_duration_ms: Option<f32>,
        cut_group: Option<i32>,
        cut_voices: Option<u32>,
        cutoff_ms: Option<f32>,
    ) -> Option<(Arc<Vec<f32>>, usize)> {
        if let Err(e) = validate_voice_schedule(
            gain,
            pan,
            max_duration_ms,
            cut_group,
            cut_voices,
            cutoff_ms,
        ) {
            eprintln!("[audio] skip: {e}");
            return None;
        }
        match self.load_clip_frames(path, max_duration_ms) {
            Ok(Some(pair)) => Some(pair),
            Ok(None) => None,
            Err(e) => {
                eprintln!("[audio] {}: {e}", path.display());
                None
            }
        }
    }

    /// Enqueue all prepared voices relative to an explicit cycle base (single lock, minimal latency).
    pub fn commit_prepared_voices_at(
        &self,
        cycle_base: u64,
        items: &[PreparedVoice],
    ) -> Result<(), String> {
        let mut voices = self
            .voices
            .write()
            .map_err(|_| "voices lock poisoned".to_string())?;
        for p in items {
            let start = cycle_base.saturating_add(p.offset_frames);
            push_prepared_voice(
                &mut voices,
                self.sample_rate,
                start,
                Arc::clone(&p.data),
                p.frame_count,
                p.gain,
                p.pan,
                p.cut_group,
                p.cut_voices,
                p.cutoff_ms,
            );
        }
        Ok(())
    }

    /// Schedule many one-shots with a single voices lock (order preserved for `cut` groups).
    #[allow(dead_code)]
    pub fn schedule_voices(&self, items: &[ScheduledVoice]) -> Result<(), String> {
        let mut prepared: Vec<(Arc<Vec<f32>>, usize, ScheduledVoice)> =
            Vec::with_capacity(items.len());
        for spec in items {
            if let Err(e) = validate_voice_schedule(
                spec.gain,
                spec.pan,
                spec.max_duration_ms,
                spec.cut_group,
                spec.cut_voices,
                spec.cutoff_ms,
            ) {
                eprintln!("[audio] skip: {e}");
                continue;
            }
            let (data, frame_count) = match self.load_clip_frames(&spec.path, spec.max_duration_ms) {
                Ok(Some(x)) => x,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("[audio] {}: {e}", spec.path.display());
                    continue;
                }
            };
            prepared.push((data, frame_count, spec.clone()));
        }

        let mut voices = self
            .voices
            .write()
            .map_err(|_| "voices lock poisoned".to_string())?;
        for (data, frame_count, spec) in prepared {
            push_prepared_voice(
                &mut voices,
                self.sample_rate,
                spec.start_frame,
                data,
                frame_count,
                spec.gain,
                spec.pan,
                spec.cut_group,
                spec.cut_voices,
                spec.cutoff_ms,
            );
        }
        Ok(())
    }

    /// Schedule a one-shot starting at global frame `start_frame`.
    #[allow(dead_code)] // kept for API symmetry; the binary uses [`schedule_voices`] each cycle
    pub fn schedule_voice(
        &self,
        start_frame: u64,
        path: &Path,
        gain: f32,
        pan: f32,
        max_duration_ms: Option<f32>,
        cut_group: Option<i32>,
        cut_voices: Option<u32>,
        cutoff_ms: Option<f32>,
    ) -> Result<(), String> {
        validate_voice_schedule(
            gain,
            pan,
            max_duration_ms,
            cut_group,
            cut_voices,
            cutoff_ms,
        )?;
        let Some((data, frame_count)) = self.load_clip_frames(path, max_duration_ms)? else {
            return Ok(());
        };
        let mut voices = self
            .voices
            .write()
            .map_err(|_| "voices lock poisoned".to_string())?;
        push_prepared_voice(
            &mut voices,
            self.sample_rate,
            start_frame,
            data,
            frame_count,
            gain,
            pan,
            cut_group,
            cut_voices,
            cutoff_ms,
        );
        Ok(())
    }

    fn clip_for_device(&self, path: &Path) -> Result<Arc<Vec<f32>>, String> {
        let key = path.to_path_buf();
        {
            let cache = self.cache.read().map_err(|_| "cache lock poisoned")?;
            if let Some(a) = cache.get(&key) {
                return Ok(Arc::clone(a));
            }
        }
        let (in_rate, in_ch, raw) = read_wav_f32(path)?;
        let stereo = if in_ch == 1 {
            mono_to_stereo(&raw)
        } else if in_ch == 2 {
            raw
        } else {
            return Err(format!(
                "{}: expected mono or stereo WAV, got {} channels",
                path.display(),
                in_ch
            ));
        };
        let device_stereo = if in_rate == self.sample_rate {
            stereo
        } else {
            resample_stereo_linear(&stereo, in_rate, self.sample_rate)
        };
        let arc = Arc::new(device_stereo);
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(key, Arc::clone(&arc));
        }
        Ok(arc)
    }
}

fn soft_clip(x: f32) -> f32 {
    x.tanh()
}

fn mono_to_stereo(m: &[f32]) -> Vec<f32> {
    let mut v = Vec::with_capacity(m.len() * 2);
    for &s in m {
        v.push(s);
        v.push(s);
    }
    v
}

/// Linear resample stereo interleaved f32.
fn resample_stereo_linear(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let in_frames = input.len() / 2;
    let out_frames = ((in_frames as u64 * out_rate as u64) / in_rate as u64).max(1) as usize;
    let mut out = vec![0.0f32; out_frames * 2];
    for of in 0..out_frames {
        let pos = (of as f64 + 0.5) * in_rate as f64 / out_rate as f64 - 0.5;
        let i0 = pos.floor() as isize;
        let frac = (pos - i0 as f64) as f32;
        for c in 0..2 {
            let get = |i: isize| -> f32 {
                let ii = i.clamp(0, in_frames as isize - 1) as usize;
                input[ii * 2 + c]
            };
            let s0 = get(i0);
            let s1 = get(i0 + 1);
            out[of * 2 + c] = s0 * (1.0 - frac) + s1 * frac;
        }
    }
    out
}

fn read_wav_f32(path: &Path) -> Result<(u32, u16, Vec<f32>), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let rate = spec.sample_rate;
    let ch = spec.channels;
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => {
            reader.samples::<f32>().filter_map(Result::ok).collect()
        }
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .filter_map(Result::ok)
            .map(|s| s as f32 / 32768.0)
            .collect(),
        (hound::SampleFormat::Int, 24) => reader
            .samples::<i32>()
            .filter_map(Result::ok)
            .map(|s| s as f32 / 8_388_608.0)
            .collect(),
        _ => {
            return Err(format!(
                "{}: unsupported WAV format (bits={}, format={:?})",
                path.display(),
                spec.bits_per_sample,
                spec.sample_format
            ))
        }
    };
    Ok((rate, ch, samples))
}

/// Hit offset in frames from cycle start; `frac` in [0,1).
pub fn hit_offset_frames(frac: f64, cycle_len: u64) -> u64 {
    if cycle_len <= 1 {
        return 0;
    }
    let x = (frac * cycle_len as f64).round() as i64;
    x.max(0).min(cycle_len as i64 - 1) as u64
}
