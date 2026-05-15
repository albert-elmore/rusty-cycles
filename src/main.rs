//! Live-coding cycle sequencer: watch a pattern file, reload on cycle boundaries, trigger WAVs.

mod audio;

use audio::PreparedVoice;
use sequencer::dsl::{parse_with_options, TrackKind};

use std::fs::File;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use serde_json::json;

/// Bold red + reset (only used when stderr is a TTY).
const CLIP_EMPH: &str = "\x1b[1;31m";
const ANSI_RESET: &str = "\x1b[0m";

use clap::{Parser, ValueEnum};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum LimiterArg {
    None,
    Soft,
}

#[derive(Parser, Debug)]
#[command(
    name = "sequencer",
    version,
    about = "Cycle sequencer with live file reload"
)]
struct Cli {
    /// Pattern file (.seq)
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Scene files; if provided, these are used instead of FILE
    #[arg(long = "scene", value_name = "FILE")]
    scenes: Vec<PathBuf>,

    /// Default BPM when the file does not set `bpm`
    #[arg(long, default_value_t = 120)]
    default_bpm: u32,

    /// Base directory for relative sample paths (default: directory containing FILE)
    #[arg(long)]
    sample_root: Option<PathBuf>,

    /// Disable watching; load file once at startup
    #[arg(long)]
    no_watch: bool,

    /// Log each cycle: expected length vs measured wall time (sanity-check BPM scheduling)
    #[arg(long)]
    verbose: bool,

    /// Delay every odd 1/8 slot by this amount (0..1, roughly 16th-note fraction)
    #[arg(long, default_value_t = 0.0)]
    swing: f64,

    /// Per-hit deterministic jitter in milliseconds (0 disables)
    #[arg(long, default_value_t = 0.0)]
    humanize_ms: f64,

    /// Apply accepted reloads only every N cycles (1 = every cycle boundary)
    #[arg(long, default_value_t = 1)]
    reload_quantum_cycles: u64,

    /// Auto-rotate scenes every N cycles (0 disables)
    #[arg(long, default_value_t = 0)]
    scene_switch_cycles: u64,

    /// Master limiter mode
    #[arg(long, value_enum, default_value_t = LimiterArg::None)]
    limiter: LimiterArg,

    /// Write scheduling trace JSONL and stop after N cycles (0 disables)
    #[arg(long, default_value_t = 0)]
    trace_cycles: u64,

    /// Path for trace JSONL output (used when --trace-cycles > 0)
    #[arg(long, default_value = "sequencer-trace.jsonl")]
    trace_file: PathBuf,

    /// Max per-hit trace lines written per cycle
    #[arg(long, default_value_t = 512)]
    trace_events_limit: usize,

    /// Enqueue voices this many frames before cycle base (0 disables lookahead)
    #[arg(long, default_value_t = 2048)]
    schedule_lookahead_frames: u64,
}

struct CyclePlan {
    prepared: Vec<PreparedVoice>,
    trace_hits: Vec<serde_json::Value>,
    prepared_count: usize,
    events_count: usize,
}

struct CycleCommitMetrics {
    commit_target: u64,
    commit_base_before: u64,
    commit_base_after: u64,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let mut trace_writer: Option<std::io::BufWriter<File>> = if cli.trace_cycles > 0 {
        let f = File::create(&cli.trace_file)
            .map_err(|e| format!("{}: {e}", cli.trace_file.display()))?;
        eprintln!(
            "[trace] enabled: {} cycle(s) -> {}",
            cli.trace_cycles,
            cli.trace_file.display()
        );
        Some(std::io::BufWriter::new(f))
    } else {
        None
    };
    let mut scene_files: Vec<PathBuf> = if !cli.scenes.is_empty() {
        cli.scenes.clone()
    } else {
        vec![cli
            .file
            .clone()
            .ok_or_else(|| "missing FILE (or pass one or more --scene)".to_string())?]
    };
    for f in &mut scene_files {
        *f = f
            .canonicalize()
            .map_err(|e| format!("{}: {e}", f.display()))?;
    }
    let mut scene_idx: usize = 0;
    let mut file = scene_files[scene_idx].clone();

    let sample_base = match &cli.sample_root {
        Some(root) => root
            .canonicalize()
            .map_err(|e| format!("{}: {e}", root.display()))?,
        None => file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    };

    eprintln!(
        "[start] scene {}/{}: {}",
        scene_idx + 1,
        scene_files.len(),
        file.display()
    );

    let source = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut active = parse_with_options(
        &source,
        cli.default_bpm,
        &sample_base,
        file.parent(),
    )
    .map_err(|e| format!("initial parse failed (line {}): {}", e.line, e.message))?;
    eprintln!(
        "[start] loaded {} track(s), bpm {}",
        active.tracks.len(),
        active.bpm
    );

    // Reload when file *contents* change (mtime is unreliable with some editors / FS setups).
    let mut last_source = source;
    let mut pending_program: Option<sequencer::dsl::Program> = None;

    let (reload_tx, reload_rx) = mpsc::channel::<()>();

    if !cli.no_watch && scene_files.len() == 1 {
        let watch_path = file.clone();
        let tx = reload_tx.clone();
        thread::spawn(move || {
            let mut debouncer = match new_debouncer(
                Duration::from_millis(200),
                move |res: DebounceEventResult| match res {
                    Ok(events) => {
                        if !events.is_empty() {
                            let _ = tx.send(());
                        }
                    }
                    Err(e) => eprintln!("[watch] {e}"),
                },
            ) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[watch] failed to start debouncer: {e}");
                    return;
                }
            };
            if let Err(e) = debouncer
                .watcher()
                .watch(&watch_path, RecursiveMode::NonRecursive)
            {
                eprintln!("[watch] {}: {e}", watch_path.display());
                return;
            }
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        });
    }

    let limiter_mode = match cli.limiter {
        LimiterArg::None => audio::LimiterMode::None,
        LimiterArg::Soft => audio::LimiterMode::Soft,
    };
    let audio = audio::Audio::try_new_with_limiter(limiter_mode)?;
    eprintln!(
        "[start] audio: {} Hz, {} ch (sample-clock scheduling, limiter={:?})",
        audio.sample_rate(),
        audio.out_channels(),
        audio.limiter_mode()
    );

    let mut next_base = audio.align_next_cycle_base(audio.cycle_len_frames(active.bpm));
    let mut cycle_index: u64 = 0;
    let mut cycle_len = audio.cycle_len_frames(active.bpm);
    let mut plan = build_cycle_plan(
        &audio,
        &active,
        &sample_base,
        cycle_index,
        cycle_len,
        &cli,
        trace_writer.is_some(),
    );
    // Prime cycle 0 immediately so playback can start; subsequent cycles are committed with lookahead.
    let init_commit_target = next_base.saturating_sub(cli.schedule_lookahead_frames);
    audio.wait_until_frame(init_commit_target);
    let init_before = audio.played_frames();
    if let Err(e) = audio.commit_prepared_voices_at(next_base, &plan.prepared) {
        eprintln!("[audio] {e}");
    }
    let init_after = audio.played_frames();
    let mut metrics = CycleCommitMetrics {
        commit_target: init_commit_target,
        commit_base_before: init_before,
        commit_base_after: init_after,
    };

    loop {
        if scene_files.len() > 1
            && cli.scene_switch_cycles > 0
            && cycle_index > 0
            && cycle_index.is_multiple_of(cli.scene_switch_cycles)
        {
            scene_idx = (scene_idx + 1) % scene_files.len();
            file = scene_files[scene_idx].clone();
            last_source.clear();
            eprintln!(
                "[scene] switched to {}/{}: {}",
                scene_idx + 1,
                scene_files.len(),
                file.display()
            );
        }

        while reload_rx.try_recv().is_ok() {}

        // Transient read errors (editor save/rename races) must not exit the process.
        match std::fs::read_to_string(&file) {
            Ok(s) => {
                // Debounce duplicate watcher bursts by content: only re-parse when bytes changed.
                if s != last_source {
                    last_source = s.clone();
                    match parse_with_options(&s, cli.default_bpm, &sample_base, file.parent()) {
                        Ok(p) => {
                            pending_program = Some(p);
                        }
                        Err(e) => eprintln!("[parse] line {}: {}", e.line, e.message),
                    }
                }
            }
            Err(e) => eprintln!(
                "[read] {}: {e} (keeping last good program; retry next cycle)",
                file.display()
            ),
        }
        let mut active_changed = false;
        if let Some(p) = pending_program.take() {
            let rq = p
                .reload_quantum_cycles
                .unwrap_or(cli.reload_quantum_cycles)
                .max(1);
            if cycle_index.is_multiple_of(rq) {
                let cycle_s = 60.0_f64 / p.bpm as f64;
                eprintln!(
                    "[reload] ok — bpm {}→{}  master {:.3}→{:.3}  swing {:.3}→{:.3}  humanize_ms {:.3}→{:.3}  rq {}→{}  tracks {}→{}  cycle {:.4}s",
                    active.bpm,
                    p.bpm,
                    active.master_gain,
                    p.master_gain,
                    active.swing.unwrap_or(cli.swing),
                    p.swing.unwrap_or(cli.swing),
                    active.humanize_ms.unwrap_or(cli.humanize_ms),
                    p.humanize_ms.unwrap_or(cli.humanize_ms),
                    active
                        .reload_quantum_cycles
                        .unwrap_or(cli.reload_quantum_cycles),
                    p.reload_quantum_cycles.unwrap_or(cli.reload_quantum_cycles),
                    active.tracks.len(),
                    p.tracks.len(),
                    cycle_s
                );
                for t in &p.tracks {
                    let TrackKind::Pattern { line, .. } = &t.kind;
                    eprintln!("    · {}  gain={}  s \"…\" (line {})", t.name, t.gain, line);
                }
                // Avoid hard-cutting currently ringing voices at reload boundary; that discontinuity
                // can produce an audible click. We switch plans at the cycle boundary instead.
                audio.clear_cache();
                active = p;
                active_changed = true;
            } else {
                pending_program = Some(p);
            }
        }
        if active_changed {
            cycle_len = audio.cycle_len_frames(active.bpm);
            plan = build_cycle_plan(
                &audio,
                &active,
                &sample_base,
                cycle_index,
                cycle_len,
                &cli,
                trace_writer.is_some(),
            );
            // Do not recommit the current cycle on reload: it can overlap previously queued
            // voices for the same boundary and create a one-cycle click.
        }
        let t_wall = Instant::now();
        let mut trace_hits = plan.trace_hits;
        let prepared_count = plan.prepared_count;
        let events_count = plan.events_count;
        let cycle_base = next_base;

        if trace_writer.is_some() {
            for h in &mut trace_hits {
                if let Some(off) = h.get("offset_frames").and_then(|v| v.as_u64()) {
                    h["scheduled_start_frame"] = json!(cycle_base.saturating_add(off));
                }
                h["commit_base_frame"] = json!(cycle_base);
            }
        }

        let cycle_end = cycle_base.saturating_add(cycle_len);

        // Prepare the next cycle while current cycle audio is playing.
        let next_cycle_index = cycle_index.wrapping_add(1);
        let next_cycle_len = audio.cycle_len_frames(active.bpm);
        let next_plan = build_cycle_plan(
            &audio,
            &active,
            &sample_base,
            next_cycle_index,
            next_cycle_len,
            &cli,
            trace_writer.is_some(),
        );
        let next_commit_target = cycle_end.saturating_sub(cli.schedule_lookahead_frames);
        audio.wait_until_frame(next_commit_target);
        let next_commit_before = audio.played_frames();
        if let Err(e) = audio.commit_prepared_voices_at(cycle_end, &next_plan.prepared) {
            eprintln!("[audio] {e}");
        }
        let next_commit_after = audio.played_frames();
        let next_metrics = CycleCommitMetrics {
            commit_target: next_commit_target,
            commit_base_before: next_commit_before,
            commit_base_after: next_commit_after,
        };

        audio.wait_until_frame(cycle_end);
        let played_after_cycle_wait = audio.played_frames();
        next_base = cycle_end;

        let clipped = audio.take_clip_frames();
        if clipped > 0 {
            let msg = format!(
                "[clip] master bus: {} output frame(s) with |mix| > 1.0",
                clipped
            );
            if io::stderr().is_terminal() {
                eprintln!("{CLIP_EMPH}{msg}{ANSI_RESET}");
            } else {
                eprintln!("{msg}");
            }
        }
        let peak = audio.take_peak_abs();
        if peak > 0.0 {
            let db = 20.0 * peak.log10();
            if db > -6.0 {
                eprintln!("[headroom] peak {:.2} dBFS this cycle", db);
            }
        }

        if let Some(w) = &mut trace_writer {
            use std::io::Write;
            for h in &trace_hits {
                let line = serde_json::to_string(h)
                    .map_err(|e| format!("trace json encode: {e}"))?;
                writeln!(w, "{line}").map_err(|e| format!("trace write: {e}"))?;
            }
            let cycle_line = serde_json::to_string(&json!({
                "kind": "cycle",
                "cycle_index": cycle_index,
                "scene_index": scene_idx,
                "scene_file": file.display().to_string(),
                "bpm": active.bpm,
                "cycle_len_frames": cycle_len,
                "next_base_target": cycle_base,
                "commit_target": metrics.commit_target,
                "commit_base_before": metrics.commit_base_before,
                "commit_base_frame": cycle_base,
                "commit_base_after": metrics.commit_base_after,
                "commit_late_frames": metrics.commit_base_before.saturating_sub(cycle_base),
                "cycle_end_target": cycle_end,
                "played_after_cycle_wait": played_after_cycle_wait,
                "drift_frames": played_after_cycle_wait as i64 - cycle_end as i64,
                "wall_ms": t_wall.elapsed().as_secs_f64() * 1000.0,
                "events_count": events_count,
                "prepared_count": prepared_count,
                "trace_hits_written": trace_hits.len(),
                "clipped_frames": clipped,
                "peak_abs": peak
            }))
            .map_err(|e| format!("trace json encode: {e}"))?;
            writeln!(w, "{cycle_line}").map_err(|e| format!("trace write: {e}"))?;
            w.flush().map_err(|e| format!("trace flush: {e}"))?;
        }

        if cli.verbose {
            let sr = audio.sample_rate().max(1) as f64;
            let expect_ms = cycle_len as f64 * 1000.0 / sr;
            let wall_ms = t_wall.elapsed().as_secs_f64() * 1000.0;
            let played = audio.played_frames();
            let drift_frames = played as i64 - cycle_end as i64;
            eprintln!(
                "[cycle] idx={} bpm={} frames={} expect={:.2}ms wall={:.2}ms drift_frames={}",
                cycle_index, active.bpm, cycle_len, expect_ms, wall_ms, drift_frames
            );
        }

        if cli.trace_cycles > 0 && cycle_index + 1 >= cli.trace_cycles {
            eprintln!(
                "[trace] captured {} cycle(s), exiting",
                cli.trace_cycles
            );
            break;
        }

        cycle_index = next_cycle_index;
        cycle_len = next_cycle_len;
        plan = next_plan;
        metrics = next_metrics;
    }
    Ok(())
}

fn build_cycle_plan(
    audio: &audio::Audio,
    active: &sequencer::dsl::Program,
    sample_base: &Path,
    cycle_index: u64,
    cycle_len: u64,
    cli: &Cli,
    trace_enabled: bool,
) -> CyclePlan {
    let mut events: Vec<(
        f64,
        PathBuf,
        String,
        f32,
        f32,
        Option<f32>,
        Option<f32>,
        Option<i32>,
        Option<u32>,
        Option<f32>,
    )> = Vec::new();
    for t in &active.tracks {
        match active.events_for_track(t, cycle_index, sample_base) {
            Ok(evs) => {
                for (frac, path, g, pan, prob, dur_ms, cut_group, cut_voices, cutoff_ms) in evs {
                    events.push((
                        frac,
                        path,
                        t.name.clone(),
                        g,
                        pan,
                        prob,
                        dur_ms,
                        cut_group,
                        cut_voices,
                        cutoff_ms,
                    ));
                }
            }
            Err(e) => eprintln!("[pattern] track {} (line {}): {}", t.name, e.line, e.message),
        }
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let swing = active.swing.unwrap_or(cli.swing);
    let humanize_ms = active.humanize_ms.unwrap_or(cli.humanize_ms);
    let bpm = active.bpm;

    let mut trace_hits: Vec<serde_json::Value> = Vec::new();
    let mut prepared: Vec<PreparedVoice> = Vec::with_capacity(events.len());
    for (frac, path, track_name, gain, pan, prob, dur_ms, cut_group, cut_voices, cutoff_ms) in events {
        if let Some(p) = prob {
            if !probability_gate(p, cycle_index, &track_name, frac, &path) {
                continue;
            }
        }
        let swung = apply_swing(frac, swing);
        let frac2 = apply_humanize(swung, humanize_ms, bpm, cycle_index, &track_name);
        let off = audio::hit_offset_frames(frac2, cycle_len);
        if gain > 1.0 {
            eprintln!("[headroom] track {track_name} schedules hit gain={gain:.3} (>1.0)");
        }
        let mut prepared_ok = false;
        if let Some((data, frame_count)) = audio.prepare_voice_for_commit(
            &path,
            gain,
            pan,
            dur_ms,
            cut_group,
            cut_voices,
            cutoff_ms,
        ) {
            prepared_ok = true;
            prepared.push(PreparedVoice {
                offset_frames: off,
                data,
                frame_count,
                gain,
                pan,
                cut_group,
                cut_voices,
                cutoff_ms,
            });
        }
        if trace_enabled && trace_hits.len() < cli.trace_events_limit {
            trace_hits.push(json!({
                "kind": "hit",
                "cycle_index": cycle_index,
                "track": track_name,
                "path": path.display().to_string(),
                "frac_raw": frac,
                "frac_swung": swung,
                "frac_final": frac2,
                "offset_frames": off,
                "prepared": prepared_ok,
                "gain": gain,
                "pan": pan,
                "prob": prob,
                "dur_ms": dur_ms,
                "cut_group": cut_group,
                "cut_voices": cut_voices,
                "cutoff_ms": cutoff_ms
            }));
        }
    }
    let prepared_count = prepared.len();
    let events_count = prepared_count.max(trace_hits.len());
    CyclePlan {
        prepared,
        trace_hits,
        prepared_count,
        events_count,
    }
}

fn probability_gate(prob: f32, cycle_index: u64, track_name: &str, frac: f64, path: &Path) -> bool {
    let p = prob.clamp(0.0, 1.0) as f64;
    if p <= 0.0 {
        return false;
    }
    if p >= 1.0 {
        return true;
    }
    let mut h = DefaultHasher::new();
    cycle_index.hash(&mut h);
    track_name.hash(&mut h);
    frac.to_bits().hash(&mut h);
    path.to_string_lossy().hash(&mut h);
    let u = (h.finish() as f64) / (u64::MAX as f64);
    u <= p
}

fn apply_swing(frac: f64, swing: f64) -> f64 {
    if swing <= 0.0 {
        return frac;
    }
    let s = swing.clamp(0.0, 1.0);
    let slot = (frac * 8.0).floor() as i64;
    if slot % 2 == 1 {
        (frac + s * (1.0 / 16.0)).min(0.999_999)
    } else {
        frac
    }
}

fn apply_humanize(frac: f64, humanize_ms: f64, bpm: u32, cycle_index: u64, track_name: &str) -> f64 {
    if humanize_ms <= 0.0 {
        return frac;
    }
    let mut h = DefaultHasher::new();
    cycle_index.hash(&mut h);
    track_name.hash(&mut h);
    frac.to_bits().hash(&mut h);
    let u = (h.finish() as f64) / (u64::MAX as f64);
    let jitter_ms = (u * 2.0 - 1.0) * humanize_ms;
    let cycle_ms = (60_000.0 / bpm.max(1) as f64).max(1.0);
    let delta = jitter_ms / cycle_ms;
    (frac + delta).clamp(0.0, 0.999_999)
}
