//! Pattern file format + mini-Tidal-style sample refs (`bd:3` = 1-based index into `sample_root/bd/*.wav`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::pat::{self, Pat};

use crate::banks;

#[derive(Debug, Clone)]
pub struct Program {
    pub bpm: u32,
    /// Global mix scale after per-track / `# gain` (default **1.0**). Set with `master <float>`.
    pub master_gain: f32,
    /// Optional file-level override for global swing amount (0..1).
    pub swing: Option<f64>,
    /// Optional file-level override for deterministic jitter in ms.
    pub humanize_ms: Option<f64>,
    /// Optional file-level override for reload apply quantization in cycles (>=1).
    pub reload_quantum_cycles: Option<u64>,
    /// If true, scheduler emits no events.
    pub mute_all: bool,
    pub tracks: Vec<Track>,
    aliases: HashMap<String, SampleSpec>,
    /// Orbits / track names muted by `silence`/`mute` lines.
    silenced: HashSet<String>,
    /// If non-empty, only these tracks are scheduled.
    soloed: HashSet<String>,
}

impl Program {
    /// Whether this track name is muted (`silence` without matching `unsilence` later in file).
    pub fn is_silenced(&self, name: &str) -> bool {
        self.silenced.contains(name)
    }

    /// Whether this track name is in the active solo set.
    pub fn is_soloed(&self, name: &str) -> bool {
        self.soloed.contains(name)
    }

    /// Resolve a pattern atom (`bd:1`, path, or alias) to a WAV path.
    pub fn resolve_token_path(
        &self,
        line: usize,
        token: &str,
        sample_root: &Path,
    ) -> Result<PathBuf, ParseError> {
        let spec = resolve_sample_token(token, &self.aliases, sample_root)
            .map_err(|msg| ParseError { line, message: msg })?;
        materialize(sample_root, spec, line)
    }

    /// Expand one cycle of a track into scheduling tuples.
    pub fn events_for_track(
        &self,
        track: &Track,
        cycle_index: u64,
        sample_root: &Path,
    ) -> Result<Vec<(f64, PathBuf, f32, f32, Option<f32>, Option<f32>, Option<i32>, Option<u32>, Option<f32>)>, ParseError> {
        if self.mute_all || self.silenced.contains(&track.name) {
            return Ok(Vec::new());
        }
        if !self.soloed.is_empty() && !self.soloed.contains(&track.name) {
            return Ok(Vec::new());
        }
        let TrackKind::Pattern {
            ast,
            gain_lane,
            pan_lane,
            prob_lane,
            dur_lane,
            legato_lane,
            cut_group_lane,
            cut_voices_lane,
            cutoff_lane,
            line,
            tempo,
        } = &track.kind;
        let raw = eval_pattern_tempo(
            ast,
            gain_lane.as_ref(),
            pan_lane.as_ref(),
            prob_lane.as_ref(),
            dur_lane.as_ref(),
            legato_lane.as_ref(),
            cut_group_lane.as_ref(),
            cut_voices_lane.as_ref(),
            cutoff_lane.as_ref(),
            tempo.clone(),
            cycle_index,
        )
        .map_err(
            |msg| ParseError {
                line: *line,
                message: msg,
            },
        )?;
        let mut out = Vec::with_capacity(raw.len());
        let cycle_ms = 60_000.0f32 / self.bpm.max(1) as f32;
        for (idx, (frac, tok, step_mul, pan, prob, dur_ms, legato, cut_group, cut_voices, cutoff_ms)) in
            raw.iter().cloned().enumerate()
        {
            let path = self.resolve_token_path(*line, &tok, sample_root)?;
            let g = track.gain * step_mul * self.master_gain;
            let legato_ms = legato.and_then(|l| {
                if raw.is_empty() {
                    return None;
                }
                let next_frac = raw.get(idx + 1).map(|x| x.0).unwrap_or(raw[0].0 + 1.0);
                let span_frac = (next_frac - frac).max(0.0) as f32;
                Some((span_frac * cycle_ms * l).max(0.0))
            });
            let dur_final = dur_ms.or(legato_ms);
            out.push((frac, path, g, pan, prob, dur_final, cut_group, cut_voices, cutoff_ms));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub name: String,
    /// Linear gain applied at mix time (default 1.0).
    pub gain: f32,
    pub kind: TrackKind,
}

/// Tidal-style temporal wrapping around the inner mini-notation pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoWrap {
    /// Pattern spans **`k` physical cycles** (one slice of the inner timeline per cycle).
    Slow(u32),
    /// **`k` inner cycles** play inside one physical cycle (polyrhythmic compression).
    Fast(u32),
}

#[derive(Debug, Clone)]
pub enum TrackKind {
    Pattern {
        ast: Pat,
        /// Optional ` # gain "…"` lane (structure must match `ast`).
        gain_lane: Option<Pat>,
        /// Optional ` # pan "…"` lane in [-1, 1] (L..R).
        pan_lane: Option<Pat>,
        /// Optional ` # prob ...` lane in [0, 1].
        prob_lane: Option<Pat>,
        /// Optional ` # dur ...` / ` # duration ...` lane in milliseconds.
        dur_lane: Option<Pat>,
        /// Optional ` # legato ...` lane (relative duration multiplier).
        legato_lane: Option<Pat>,
        /// Optional ` # cut ...` first argument lane: group id >= 0.
        cut_group_lane: Option<Pat>,
        /// Optional ` # cut ...` second argument lane: voices >= 1.
        cut_voices_lane: Option<Pat>,
        /// Optional ` # cutoff ...` lane in milliseconds.
        cutoff_lane: Option<Pat>,
        /// Optional `slow k $` / `fast k $` before `s`.
        tempo: Option<TempoWrap>,
        /// Source line for error messages.
        line: usize,
    },
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone)]
enum SampleSpec {
    Path(PathBuf),
    Bank { bank: String, index: u32 },
}

enum PendingTrack {
    Pattern {
        line_no: usize,
        name: String,
        gain: f32,
        ast: Pat,
        gain_lane: Option<Pat>,
        pan_lane: Option<Pat>,
        prob_lane: Option<Pat>,
        dur_lane: Option<Pat>,
        legato_lane: Option<Pat>,
        cut_group_lane: Option<Pat>,
        cut_voices_lane: Option<Pat>,
        cutoff_lane: Option<Pat>,
        tempo: Option<TempoWrap>,
    },
}

/// Parse `bank:index` with **1-based** index (e.g. `bd:3`). Returns `None` if not that shape.
pub fn parse_bank_ref(token: &str) -> Option<(String, u32)> {
    let (left, right) = token.rsplit_once(':')?;
    if left.is_empty() || right.is_empty() {
        return None;
    }
    if !right.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let idx: u32 = right.parse().ok()?;
    if idx == 0 {
        return None;
    }
    Some((left.to_string(), idx))
}

/// If `token` names a **directory** `<sample_root>/<token>/`, treat as bank index **1** (first `.wav`).
fn bank_from_dir_if_present(token: &str, sample_root: &Path) -> Option<SampleSpec> {
    if token.is_empty() || token.contains(':') || token.contains('/') || token.contains('\\') {
        return None;
    }
    let dir = sample_root.join(token);
    if dir.is_dir() {
        Some(SampleSpec::Bank {
            bank: token.to_string(),
            index: 1,
        })
    } else {
        None
    }
}

fn parse_sample_token_raw(token: &str, sample_root: Option<&Path>) -> Result<SampleSpec, String> {
    if let Some((bank, index)) = parse_bank_ref(token) {
        return Ok(SampleSpec::Bank { bank, index });
    }
    if let Some(root) = sample_root {
        if let Some(spec) = bank_from_dir_if_present(token, root) {
            return Ok(spec);
        }
    }
    Ok(SampleSpec::Path(PathBuf::from(token)))
}

fn resolve_sample_token(
    token: &str,
    aliases: &HashMap<String, SampleSpec>,
    sample_root: &Path,
) -> Result<SampleSpec, String> {
    if let Some(spec) = aliases.get(token) {
        return Ok(spec.clone());
    }
    parse_sample_token_raw(token, Some(sample_root))
}

fn materialize(sample_root: &Path, spec: SampleSpec, line: usize) -> Result<PathBuf, ParseError> {
    match spec {
        SampleSpec::Path(p) => Ok(resolve_sample_path(sample_root, &p)),
        SampleSpec::Bank { bank, index } => banks::resolve_bank_sample(sample_root, &bank, index)
            .map_err(|msg| ParseError { line, message: msg }),
    }
}

fn skip_ws_bytes(line: &str, i: &mut usize) {
    let b = line.as_bytes();
    while *i < b.len() && b[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn parse_word<'a>(line: &'a str, i: &mut usize) -> Option<&'a str> {
    skip_ws_bytes(line, i);
    let b = line.as_bytes();
    let start = *i;
    while *i < b.len() && !b[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if start == *i {
        None
    } else {
        Some(&line[start..*i])
    }
}

fn parse_quoted_pat(line: &str, i: &mut usize, line_no: usize) -> Result<String, ParseError> {
    parse_quoted_string(line, i, line_no, "pattern")
}

fn parse_quoted_string(
    line: &str,
    i: &mut usize,
    line_no: usize,
    ctx: &str,
) -> Result<String, ParseError> {
    skip_ws_bytes(line, i);
    let b = line.as_bytes();
    if *i >= b.len() || b[*i] != b'"' {
        return Err(ParseError {
            line: line_no,
            message: format!("{ctx} requires an opening double quote (\")"),
        });
    }
    *i += 1;
    let start = *i;
    while *i < b.len() && b[*i] != b'"' {
        *i += 1;
    }
    if *i >= b.len() {
        return Err(ParseError {
            line: line_no,
            message: format!("unclosed string in {ctx}"),
        });
    }
    let inner = line[start..*i].to_string();
    *i += 1;
    Ok(inner)
}

fn expect_dollar(line: &str, i: &mut usize, line_no: usize, after: &str) -> Result<(), ParseError> {
    skip_ws_bytes(line, i);
    let b = line.as_bytes();
    if *i >= b.len() || b[*i] != b'$' {
        return Err(ParseError {
            line: line_no,
            message: format!("`{after}` must be followed by `$`"),
        });
    }
    *i += 1;
    Ok(())
}

/// Inline `let` bindings into mini-notation text (whole identifiers only).
fn expand_lets_in_mini_pattern(s: &str, lets: &HashMap<String, String>) -> String {
    if lets.is_empty() {
        return s.to_string();
    }
    let mut keys: Vec<&str> = lets.keys().map(String::as_str).collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut out = String::with_capacity(s.len() + 16);
    let mut i = 0usize;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        let w = c.len_utf8();
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += w;
            while i < s.len() {
                let c2 = s[i..].chars().next().unwrap();
                if c2.is_ascii_alphanumeric() || c2 == '_' {
                    i += c2.len_utf8();
                } else {
                    break;
                }
            }
            let ident = &s[start..i];
            if let Some(rep) = lets.get(ident) {
                out.push_str(rep);
            } else {
                out.push_str(ident);
            }
            continue;
        }
        out.push(c);
        i += w;
    }
    out
}

const MAX_INCLUDE_DEPTH: usize = 32;

/// Expand `include "path"` / `include path` lines relative to `base`. Detects cycles.
fn expand_include_directives(
    source: &str,
    include_base: Option<&Path>,
    stack: &mut Vec<PathBuf>,
) -> Result<String, ParseError> {
    let Some(base) = include_base else {
        for (idx, raw) in source.lines().enumerate() {
            let t = strip_comment(raw).trim();
            if t
                .split_whitespace()
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("include"))
            {
                return Err(ParseError {
                    line: idx + 1,
                    message: "`include` only works when the pattern file is loaded from disk (path known); relative paths resolve next to that `.seq` file".into(),
                });
            }
        }
        return Ok(source.to_string());
    };

    if stack.len() > MAX_INCLUDE_DEPTH {
        return Err(ParseError {
            line: 1,
            message: format!("include nesting exceeded {MAX_INCLUDE_DEPTH} levels"),
        });
    }

    let mut out = String::new();
    for (idx, raw) in source.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = strip_comment(raw).trim();
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(head) = parts.next() else {
            out.push('\n');
            continue;
        };
        if head.eq_ignore_ascii_case("include") {
            let path_tok = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "include requires a path, e.g. include \"kit.seq\" or include kit.seq".into(),
            })?;
            let path_str = if let Some(inner) = path_tok.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                if inner.contains('"') {
                    return Err(ParseError {
                        line: line_no,
                        message: "include path: use a single quoted token without embedded quotes".into(),
                    });
                }
                inner.to_string()
            } else {
                path_tok.to_string()
            };
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after include path".into(),
                });
            }
            let path = resolve_sample_path(base, Path::new(&path_str));
            let canonical = std::fs::canonicalize(&path).map_err(|e| ParseError {
                line: line_no,
                message: format!("include {}: {e}", path.display()),
            })?;
            if stack.contains(&canonical) {
                return Err(ParseError {
                    line: line_no,
                    message: format!("include cycle: {}", canonical.display()),
                });
            }
            stack.push(canonical.clone());
            let inner_src = std::fs::read_to_string(&canonical).map_err(|e| ParseError {
                line: line_no,
                message: format!("include {}: {e}", canonical.display()),
            })?;
            let parent = canonical
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| base.to_path_buf());
            let expanded = expand_include_directives(&inner_src, Some(&parent), stack)?;
            stack.pop();
            out.push_str(&expanded);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(raw);
            out.push('\n');
        }
    }
    Ok(out)
}

fn parse_optional_tempo_prefix(
    line: &str,
    i: &mut usize,
    line_no: usize,
) -> Result<Option<TempoWrap>, ParseError> {
    let save = *i;
    let word = match parse_word(line, i) {
        Some(w) => w,
        None => return Ok(None),
    };
    let is_slow = word.eq_ignore_ascii_case("slow");
    let is_fast = word.eq_ignore_ascii_case("fast");
    if !is_slow && !is_fast {
        *i = save;
        return Ok(None);
    }
    let n_str = parse_word(line, i).ok_or_else(|| ParseError {
        line: line_no,
        message: "slow/fast requires a positive integer".into(),
    })?;
    let n: u32 = n_str.parse().map_err(|_| ParseError {
        line: line_no,
        message: format!("invalid slow/fast factor: {n_str}"),
    })?;
    if n < 1 {
        return Err(ParseError {
            line: line_no,
            message: "slow/fast factor must be >= 1".into(),
        });
    }
    skip_ws_bytes(line, i);
    let b = line.as_bytes();
    if *i >= b.len() || b[*i] != b'$' {
        return Err(ParseError {
            line: line_no,
            message: "slow/fast N must be followed by `$`".into(),
        });
    }
    *i += 1;
    Ok(Some(if is_slow {
        TempoWrap::Slow(n)
    } else {
        TempoWrap::Fast(n)
    }))
}

#[derive(Clone, Copy)]
enum PrefixTransform {
    Rev,
    Pal,
    Rot(i32),
    EveryRev(u32),
    EveryPal(u32),
    EveryRot(u32, i32),
}

struct ParsedSound {
    tempo: Option<TempoWrap>,
    transform_prefixes: Vec<PrefixTransform>,
    inner: String,
    gain_lane: Option<Pat>,
    pan_lane: Option<Pat>,
    prob_lane: Option<Pat>,
    dur_lane: Option<Pat>,
    legato_lane: Option<Pat>,
    cut_group_lane: Option<Pat>,
    cut_voices_lane: Option<Pat>,
    cutoff_lane: Option<Pat>,
}

/// Right-associative `$` chain: `rev $ pal $ s "…"` → outer `rev` wraps inner `pal(sound)`.
fn parse_sound_clause(
    line: &str,
    i: &mut usize,
    line_no: usize,
    lets: &HashMap<String, String>,
) -> Result<ParsedSound, ParseError> {
    skip_ws_bytes(line, i);
    let save = *i;
    let word = parse_word(line, i).ok_or_else(|| ParseError {
        line: line_no,
        message: "expected `s \"…\"` or combinator (`slow`/`fast` N `$`, `rev` `$`, `pal` `$`, …)".into(),
    })?;

    if word.eq_ignore_ascii_case("s") {
        *i = save;
        return parse_s_clause(line, i, line_no, lets);
    }

    if word.eq_ignore_ascii_case("slow") || word.eq_ignore_ascii_case("fast") {
        *i = save;
        let tempo = parse_optional_tempo_prefix(line, i, line_no)?.ok_or_else(|| ParseError {
            line: line_no,
            message: "internal: expected slow/fast".into(),
        })?;
        let mut ps = parse_sound_clause(line, i, line_no, lets)?;
        if ps.tempo.replace(tempo).is_some() {
            return Err(ParseError {
                line: line_no,
                message: "only one `slow`/`fast` per `$` chain".into(),
            });
        }
        return Ok(ps);
    }

    if word.eq_ignore_ascii_case("rev") {
        expect_dollar(line, i, line_no, "rev")?;
        let mut ps = parse_sound_clause(line, i, line_no, lets)?;
        ps.transform_prefixes.push(PrefixTransform::Rev);
        return Ok(ps);
    }
    if word.eq_ignore_ascii_case("pal") {
        expect_dollar(line, i, line_no, "pal")?;
        let mut ps = parse_sound_clause(line, i, line_no, lets)?;
        ps.transform_prefixes.push(PrefixTransform::Pal);
        return Ok(ps);
    }
    if word.eq_ignore_ascii_case("rot") {
        let amt = parse_word(line, i).ok_or_else(|| ParseError {
            line: line_no,
            message: "`rot` requires an integer amount".into(),
        })?;
        let n: i32 = amt.parse().map_err(|_| ParseError {
            line: line_no,
            message: format!("invalid rot amount: {amt}"),
        })?;
        expect_dollar(line, i, line_no, "rot")?;
        let mut ps = parse_sound_clause(line, i, line_no, lets)?;
        ps.transform_prefixes.push(PrefixTransform::Rot(n));
        return Ok(ps);
    }
    if word.eq_ignore_ascii_case("every") {
        let n_str = parse_word(line, i).ok_or_else(|| ParseError {
            line: line_no,
            message: "`every` requires an integer >= 1".into(),
        })?;
        let n: u32 = n_str.parse().map_err(|_| ParseError {
            line: line_no,
            message: format!("invalid every count: {n_str}"),
        })?;
        if n < 1 {
            return Err(ParseError {
                line: line_no,
                message: "`every` count must be >= 1".into(),
            });
        }
        let tfm = parse_word(line, i).ok_or_else(|| ParseError {
            line: line_no,
            message: "`every` requires a transform (`rev`, `pal`, `rot`)".into(),
        })?;
        let wrapped = if tfm.eq_ignore_ascii_case("rev") {
            PrefixTransform::EveryRev(n)
        } else if tfm.eq_ignore_ascii_case("pal") {
            PrefixTransform::EveryPal(n)
        } else if tfm.eq_ignore_ascii_case("rot") {
            let amt = parse_word(line, i).ok_or_else(|| ParseError {
                line: line_no,
                message: "`every ... rot` requires an integer amount".into(),
            })?;
            let r: i32 = amt.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid rot amount: {amt}"),
            })?;
            PrefixTransform::EveryRot(n, r)
        } else {
            return Err(ParseError {
                line: line_no,
                message: format!("unsupported `every` transform: {tfm} (use rev|pal|rot)"),
            });
        };
        expect_dollar(line, i, line_no, "every")?;
        let mut ps = parse_sound_clause(line, i, line_no, lets)?;
        ps.transform_prefixes.push(wrapped);
        return Ok(ps);
    }

    *i = save;
    Err(ParseError {
        line: line_no,
        message: format!(
            "unknown combinator `{word}` — expected `rev`, `pal`, `rot`, `every`, `slow`, `fast`, or `s`"
        ),
    })
}

fn parse_s_clause(
    line: &str,
    i: &mut usize,
    line_no: usize,
    lets: &HashMap<String, String>,
) -> Result<ParsedSound, ParseError> {
    let sound_kw = parse_word(line, i).ok_or_else(|| ParseError {
        line: line_no,
        message: "expected `s \"…\"`".into(),
    })?;
    if !sound_kw.eq_ignore_ascii_case("s") {
        return Err(ParseError {
            line: line_no,
            message: format!("expected `s`, got `{sound_kw}`"),
        });
    }
    let inner_raw = parse_quoted_pat(line, i, line_no)?;
    let inner = expand_lets_in_mini_pattern(&inner_raw, lets);

    let mut gain_lane: Option<Pat> = None;
    let mut pan_lane: Option<Pat> = None;
    let mut prob_lane: Option<Pat> = None;
    let mut dur_lane: Option<Pat> = None;
    let mut legato_lane: Option<Pat> = None;
    let mut cut_group_lane: Option<Pat> = None;
    let mut cut_voices_lane: Option<Pat> = None;
    let mut cutoff_lane: Option<Pat> = None;
    loop {
        skip_ws_bytes(line, i);
        if *i >= line.len() {
            break;
        }
        let b = line.as_bytes();
        if b[*i] != b'#' {
            return Err(ParseError {
                line: line_no,
                message:
                    "unexpected text after pattern (use ` # gain|pan|prob|dur|duration|legato|cut|cutoff ...` lanes)"
                        .into(),
            });
        }
        *i += 1;
        let lane = parse_word(line, i).ok_or_else(|| ParseError {
            line: line_no,
            message: "`#` must be followed by a supported lane (`gain`, `pan`, `prob`, `dur`, `duration`, `legato`, `cut`, `cutoff`)".into(),
        })?;
        if lane.eq_ignore_ascii_case("gain") {
            let g_ast = parse_lane_pattern(line, i, line_no, "gain", lets)?;
            if gain_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # gain \"…\"` lane per track for now".into(),
                });
            }
            gain_lane = Some(g_ast);
        } else if lane.eq_ignore_ascii_case("pan") {
            let p_ast = parse_lane_pattern(line, i, line_no, "pan", lets)?;
            if pan_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # pan \"…\"` lane per track for now".into(),
                });
            }
            pan_lane = Some(p_ast);
        } else if lane.eq_ignore_ascii_case("prob") {
            let p_ast = parse_lane_pattern(line, i, line_no, "prob", lets)?;
            if prob_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # prob ...` lane per track for now".into(),
                });
            }
            prob_lane = Some(p_ast);
        } else if lane.eq_ignore_ascii_case("dur") || lane.eq_ignore_ascii_case("duration") {
            let d_ast = parse_lane_pattern(line, i, line_no, "dur", lets)?;
            if dur_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # dur ...` / ` # duration ...` lane per track for now".into(),
                });
            }
            dur_lane = Some(d_ast);
        } else if lane.eq_ignore_ascii_case("legato") {
            let l_ast = parse_lane_pattern(line, i, line_no, "legato", lets)?;
            if legato_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # legato ...` lane per track for now".into(),
                });
            }
            legato_lane = Some(l_ast);
        } else if lane.eq_ignore_ascii_case("cut") {
            let c_ast = parse_lane_pattern(line, i, line_no, "cut", lets)?;
            if cut_group_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # cut ...` lane per track for now".into(),
                });
            }
            cut_group_lane = Some(c_ast);
            let save_lane = *i;
            skip_ws_bytes(line, i);
            if *i < line.len() && line.as_bytes()[*i] != b'#' {
                let voices_ast = parse_lane_pattern(line, i, line_no, "cut voices", lets)?;
                cut_voices_lane = Some(voices_ast);
            } else {
                *i = save_lane;
            }
        } else if lane.eq_ignore_ascii_case("cutoff") {
            let c_ast = parse_lane_pattern(line, i, line_no, "cutoff", lets)?;
            if cutoff_lane.is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "only one ` # cutoff ...` lane per track for now".into(),
                });
            }
            cutoff_lane = Some(c_ast);
        } else {
            return Err(ParseError {
                line: line_no,
                message: format!("unsupported lane after `#`: {lane}"),
            });
        }
    }

    Ok(ParsedSound {
        tempo: None,
        transform_prefixes: Vec::new(),
        inner,
        gain_lane,
        pan_lane,
        prob_lane,
        dur_lane,
        legato_lane,
        cut_group_lane,
        cut_voices_lane,
        cutoff_lane,
    })
}

/// Apply optional `slow`/`fast` then run mini-notation eval (same cycle window [0,1) for inner).
fn eval_pattern_tempo(
    ast: &Pat,
    gain_lane: Option<&Pat>,
    pan_lane: Option<&Pat>,
    prob_lane: Option<&Pat>,
    dur_lane: Option<&Pat>,
    legato_lane: Option<&Pat>,
    cut_group_lane: Option<&Pat>,
    cut_voices_lane: Option<&Pat>,
    cutoff_lane: Option<&Pat>,
    tempo: Option<TempoWrap>,
    cycle_index: u64,
) -> Result<Vec<(f64, String, f32, f32, Option<f32>, Option<f32>, Option<f32>, Option<i32>, Option<u32>, Option<f32>)>, String> {
    match tempo {
        None => pat::eval_with_control_lanes(
            ast,
            gain_lane,
            pan_lane,
            prob_lane,
            dur_lane,
            legato_lane,
            cut_group_lane,
            cut_voices_lane,
            cutoff_lane,
            cycle_index,
            0.0,
            1.0,
        ),
        Some(TempoWrap::Slow(k)) => {
            let k_u = u64::from(k);
            let k_f = k_u as f64;
            let lc = cycle_index / k_u;
            let slot = cycle_index % k_u;
            let chunk = pat::eval_with_control_lanes(
                ast,
                gain_lane,
                pan_lane,
                prob_lane,
                dur_lane,
                legato_lane,
                cut_group_lane,
                cut_voices_lane,
                cutoff_lane,
                lc,
                0.0,
                1.0,
            )?;
            let mut out = Vec::new();
            for (phase, tok, step_mul, pan, prob, dur, legato, cut_g, cut_v, cutoff) in chunk {
                let slot_hit = (phase * k_f).floor() as u64;
                if slot_hit != slot {
                    continue;
                }
                let phys = (phase * k_f) % 1.0;
                out.push((phys, tok, step_mul, pan, prob, dur, legato, cut_g, cut_v, cutoff));
            }
            Ok(out)
        }
        Some(TempoWrap::Fast(k)) => {
            let k_u = u64::from(k);
            let k_f = k_u as f64;
            let mut acc = Vec::new();
            for j in 0..k_u {
                let lc = cycle_index.saturating_mul(k_u).saturating_add(j);
                let chunk = pat::eval_with_control_lanes(
                    ast,
                    gain_lane,
                    pan_lane,
                    prob_lane,
                    dur_lane,
                    legato_lane,
                    cut_group_lane,
                    cut_voices_lane,
                    cutoff_lane,
                    lc,
                    0.0,
                    1.0,
                )?;
                let t0 = j as f64 / k_f;
                let w = 1.0 / k_f;
                for (frac, tok, step_mul, pan, prob, dur, legato, cut_g, cut_v, cutoff) in chunk {
                    let phys = t0 + frac * w;
                    acc.push((phys, tok, step_mul, pan, prob, dur, legato, cut_g, cut_v, cutoff));
                }
            }
            Ok(acc)
        }
    }
}

fn parse_track_line(
    line: &str,
    line_no: usize,
    _aliases: &HashMap<String, SampleSpec>,
    _sample_root: &Path,
    lets: &HashMap<String, String>,
) -> Result<PendingTrack, ParseError> {
    let mut i = 0usize;
    let head = parse_word(line, &mut i).ok_or_else(|| ParseError {
        line: line_no,
        message: "empty line".into(),
    })?;
    if !head.eq_ignore_ascii_case("track") {
        return Err(ParseError {
            line: line_no,
            message: format!("internal: expected track, got {head}"),
        });
    }

    let name = parse_word(line, &mut i)
        .ok_or_else(|| ParseError {
            line: line_no,
            message: "track requires a name".into(),
        })?
        .to_string();

    let mut gain = 1.0f32;
    let save = i;
    if let Some(w) = parse_word(line, &mut i) {
        if w.eq_ignore_ascii_case("gain") {
            let g_str = parse_word(line, &mut i).ok_or_else(|| ParseError {
                line: line_no,
                message: "gain requires a number".into(),
            })?;
            gain = g_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid gain: {g_str}"),
            })?;
            if !gain.is_finite() || gain < 0.0 {
                return Err(ParseError {
                    line: line_no,
                    message: format!("gain must be finite and ≥ 0: {gain}"),
                });
            }
        } else {
            i = save;
        }
    }

    let ps = parse_sound_clause(line, &mut i, line_no, lets)?;

    let pat_err = |msg: String| ParseError {
        line: line_no,
        message: msg,
    };
    let mut ast = pat::parse_pattern(&ps.inner).map_err(pat_err)?;
    for t in ps.transform_prefixes.into_iter().rev() {
        ast = match t {
            PrefixTransform::Rev => Pat::Rev(Box::new(ast)),
            PrefixTransform::Pal => Pat::Pal(Box::new(ast)),
            PrefixTransform::Rot(n) => Pat::Rot(n, Box::new(ast)),
            PrefixTransform::EveryPal(n) => {
                let base = ast.clone();
                Pat::Every(n, Box::new(Pat::Pal(Box::new(ast))), Box::new(base))
            }
            PrefixTransform::EveryRev(n) => {
                let base = ast.clone();
                Pat::Every(n, Box::new(Pat::Rev(Box::new(ast))), Box::new(base))
            }
            PrefixTransform::EveryRot(n, r) => {
                let base = ast.clone();
                Pat::Every(n, Box::new(Pat::Rot(r, Box::new(ast))), Box::new(base))
            }
        };
    }

    skip_ws_bytes(line, &mut i);
    if i < line.len() {
        return Err(ParseError {
            line: line_no,
            message: format!(
                "unexpected trailing input after pattern line (at byte {i}); check for missing `$` between combinators"
            ),
        });
    }

    Ok(PendingTrack::Pattern {
        line_no,
        name,
        gain,
        ast,
        gain_lane: ps.gain_lane,
        pan_lane: ps.pan_lane,
        prob_lane: ps.prob_lane,
        dur_lane: ps.dur_lane,
        legato_lane: ps.legato_lane,
        cut_group_lane: ps.cut_group_lane,
        cut_voices_lane: ps.cut_voices_lane,
        cutoff_lane: ps.cutoff_lane,
        tempo: ps.tempo,
    })
}

fn parse_lane_pattern(
    line: &str,
    i: &mut usize,
    line_no: usize,
    lane_name: &str,
    lets: &HashMap<String, String>,
) -> Result<Pat, ParseError> {
    skip_ws_bytes(line, i);
    let b = line.as_bytes();
    if *i >= b.len() || b[*i] != b'"' {
        return Err(ParseError {
            line: line_no,
            message: format!("{lane_name} lane requires a quoted pattern (e.g. \"0.5\")"),
        });
    }
    let inner_raw = parse_quoted_pat(line, i, line_no)?;
    let inner = expand_lets_in_mini_pattern(&inner_raw, lets);
    pat::parse_pattern(&inner).map_err(|msg| ParseError {
        line: line_no,
        message: msg,
    })
}

fn parse_let_binding(line: &str, line_no: usize) -> Result<(String, String), ParseError> {
    let mut i = 0usize;
    let kw = parse_word(line, &mut i).ok_or_else(|| ParseError {
        line: line_no,
        message: "internal: empty `let` line".into(),
    })?;
    if !kw.eq_ignore_ascii_case("let") {
        return Err(ParseError {
            line: line_no,
            message: "internal: expected `let`".into(),
        });
    }
    let name = parse_word(line, &mut i)
        .ok_or_else(|| ParseError {
            line: line_no,
            message: "`let` requires a name, e.g. let kicks \"bd sn\"".into(),
        })?
        .to_string();
    if name.is_empty()
        || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ParseError {
            line: line_no,
            message: format!(
                "`let` name must be an identifier ([a-zA-Z_][a-zA-Z0-9_]*), got {name:?}"
            ),
        });
    }
    let body = parse_quoted_string(line, &mut i, line_no, "let binding")?;
    skip_ws_bytes(line, &mut i);
    if i < line.len() {
        return Err(ParseError {
            line: line_no,
            message: "extra tokens after `let` binding (expected only `let name \"…\"`)".into(),
        });
    }
    Ok((name, body))
}

fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut in_quote = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                in_quote = !in_quote;
                i += 1;
            }
            b'-' if !in_quote && i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                return &line[..i];
            }
            _ => i += 1,
        }
    }
    line
}

/// Optional Tidal-style `dN $` prefix (`d1`, `d2`, …); implicit `track d1` when line starts with `s` / `slow` / `fast`.
fn split_dn_prefix(line: &str) -> (Option<String>, String) {
    let t = line.trim_start();
    let b = t.as_bytes();
    if b.is_empty() || !matches!(b[0], b'd' | b'D') {
        return (None, t.to_string());
    }
    let mut i = 1usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == 1 {
        return (None, t.to_string());
    }
    let orbit = format!("d{}", &t[1..i]);
    let rest = t[i..].trim_start();
    let Some(after) = rest.strip_prefix('$') else {
        return (None, t.to_string());
    };
    (Some(orbit), after.trim_start().to_string())
}

fn line_needs_implicit_track(stripped: &str) -> bool {
    let mut it = stripped.split_whitespace();
    let Some(head) = it.next() else {
        return false;
    };
    if head.eq_ignore_ascii_case("gain") {
        let (Some(g), Some(next)) = (it.next(), it.next()) else {
            return false;
        };
        if g.parse::<f32>().is_err() {
            return false;
        }
        if next.eq_ignore_ascii_case("s") {
            return true;
        }
        if next.eq_ignore_ascii_case("slow") || next.eq_ignore_ascii_case("fast") {
            let (Some(n), Some(dollar), Some(s_kw)) = (it.next(), it.next(), it.next()) else {
                return false;
            };
            return n.chars().all(|c| c.is_ascii_digit())
                && n.parse::<u32>().is_ok()
                && dollar == "$"
                && s_kw.eq_ignore_ascii_case("s");
        }
        return false;
    }
    if head.eq_ignore_ascii_case("rev")
        || head.eq_ignore_ascii_case("pal")
        || head.eq_ignore_ascii_case("rot")
        || head.eq_ignore_ascii_case("every")
    {
        return true;
    }
    if head.eq_ignore_ascii_case("s") {
        return true;
    }
    if head.eq_ignore_ascii_case("slow") || head.eq_ignore_ascii_case("fast") {
        let (Some(n), Some(dollar)) = (it.next(), it.next()) else {
            return false;
        };
        return n.chars().all(|c| c.is_ascii_digit()) && n.parse::<u32>().is_ok() && dollar == "$";
    }
    false
}

fn normalize_seq_line(line: &str) -> String {
    let (orbit, stripped) = split_dn_prefix(line);
    let head = stripped.split_whitespace().next().unwrap_or("");
    if head.eq_ignore_ascii_case("track")
        || head.eq_ignore_ascii_case("bpm")
        || head.eq_ignore_ascii_case("master")
        || head.eq_ignore_ascii_case("swing")
        || head.eq_ignore_ascii_case("humanize_ms")
        || head.eq_ignore_ascii_case("humanize")
        || head.eq_ignore_ascii_case("reload_quantum_cycles")
        || head.eq_ignore_ascii_case("quantum")
        || head.eq_ignore_ascii_case("include")
        || head.eq_ignore_ascii_case("alias")
        || head.eq_ignore_ascii_case("silence")
        || head.eq_ignore_ascii_case("unsilence")
        || head.eq_ignore_ascii_case("mute")
        || head.eq_ignore_ascii_case("solo")
        || head.eq_ignore_ascii_case("let")
    {
        stripped
    } else if line_needs_implicit_track(&stripped) {
        let name = orbit.unwrap_or_else(|| "d1".into());
        format!("track {} {}", name, stripped.trim_start())
    } else {
        stripped
    }
}
/// `[]` depth outside `"…"`, and no unclosed `"` — Tidal-style multiline `stack [ … ]` / quoted blocks.
fn statement_boundary_complete(s: &str) -> bool {
    let mut quote = false;
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '"' => quote = !quote,
            '[' if !quote => depth += 1,
            ']' if !quote => depth -= 1,
            _ => {}
        }
    }
    depth == 0 && !quote
}

/// Merge physical lines until brackets/quotes balance (no `\` continuation).
fn merge_statement_lines(source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut acc = String::new();
    let mut start_line = 1usize;

    for (idx, raw) in source.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = strip_comment(raw).trim();
        if trimmed.is_empty() {
            if acc.is_empty() {
                continue;
            }
            continue;
        }
        if acc.is_empty() {
            start_line = line_no;
        } else {
            acc.push(' ');
        }
        acc.push_str(trimmed);
        if statement_boundary_complete(&acc) {
            out.push((start_line, std::mem::take(&mut acc)));
        }
    }

    if !acc.is_empty() {
        out.push((start_line, acc));
    }
    out
}

/// Parse a `.seq` file and resolve sample paths against `sample_root`.
/// `include` lines are only expanded when [`parse_with_options`] is used with a file directory.
pub fn parse(source: &str, default_bpm: u32, sample_root: &Path) -> Result<Program, ParseError> {
    parse_with_options(source, default_bpm, sample_root, None)
}

/// Like [`parse`], but resolves `include` relative to `include_base` (usually the `.seq` file's parent folder).
pub fn parse_with_options(
    source: &str,
    default_bpm: u32,
    sample_root: &Path,
    include_base: Option<&Path>,
) -> Result<Program, ParseError> {
    let expanded = expand_include_directives(source, include_base, &mut Vec::new())?;
    let source = expanded.as_str();

    let mut bpm = default_bpm;
    let mut master_gain = 1.0f32;
    let mut swing: Option<f64> = None;
    let mut humanize_ms: Option<f64> = None;
    let mut reload_quantum_cycles: Option<u64> = None;
    let mut mute_all = false;
    let mut aliases: HashMap<String, SampleSpec> = HashMap::new();
    let mut silenced: HashSet<String> = HashSet::new();
    let mut soloed: HashSet<String> = HashSet::new();
    let mut pending_tracks: Vec<PendingTrack> = Vec::new();
    let mut lets: HashMap<String, String> = HashMap::new();

    for (line_no, raw_line) in merge_statement_lines(source) {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed
            .split_whitespace()
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("track"))
        {
            return Err(ParseError {
                line: line_no,
                message:
                    "explicit `track` syntax removed; use `dN $ ...` (or bare `s ...` for d1)"
                        .into(),
            });
        }
        let line = normalize_seq_line(trimmed);

        let mut parts = line.split_whitespace();
        let Some(head) = parts.next() else { continue };

        if head.eq_ignore_ascii_case("let") {
            let (name, body) = parse_let_binding(line.trim(), line_no)?;
            lets.insert(name, body);
            continue;
        }

        if head.eq_ignore_ascii_case("bpm") {
            let n = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "bpm requires a number".into(),
            })?;
            let v: u32 = n.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid bpm: {n}"),
            })?;
            if !(1..=400).contains(&v) {
                return Err(ParseError {
                    line: line_no,
                    message: format!("bpm out of range (1–400): {v}"),
                });
            }
            bpm = v;
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after bpm".into(),
                });
            }
            continue;
        }

        if head.eq_ignore_ascii_case("master") {
            let g_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "master requires a number (e.g. master 0.2)".into(),
            })?;
            let g: f32 = g_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid master gain: {g_str}"),
            })?;
            if !g.is_finite() || g < 0.0 {
                return Err(ParseError {
                    line: line_no,
                    message: format!("master gain must be finite and ≥ 0: {g}"),
                });
            }
            master_gain = g;
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after master".into(),
                });
            }
            continue;
        }

        if head.eq_ignore_ascii_case("swing") {
            let s_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "swing requires a number in [0,1]".into(),
            })?;
            let s: f64 = s_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid swing: {s_str}"),
            })?;
            if !s.is_finite() || !(0.0..=1.0).contains(&s) {
                return Err(ParseError {
                    line: line_no,
                    message: format!("swing must be in [0,1]: {s}"),
                });
            }
            swing = Some(s);
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after swing".into(),
                });
            }
            continue;
        }

        if head.eq_ignore_ascii_case("humanize_ms") {
            let h_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "humanize_ms requires a non-negative number".into(),
            })?;
            let h: f64 = h_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid humanize_ms: {h_str}"),
            })?;
            if !h.is_finite() || h < 0.0 {
                return Err(ParseError {
                    line: line_no,
                    message: format!("humanize_ms must be finite and >= 0: {h}"),
                });
            }
            humanize_ms = Some(h);
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after humanize_ms".into(),
                });
            }
            continue;
        }
        if head.eq_ignore_ascii_case("humanize") {
            let h_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "humanize requires a non-negative number".into(),
            })?;
            let h: f64 = h_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid humanize: {h_str}"),
            })?;
            if !h.is_finite() || h < 0.0 {
                return Err(ParseError {
                    line: line_no,
                    message: format!("humanize must be finite and >= 0: {h}"),
                });
            }
            humanize_ms = Some(h);
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after humanize".into(),
                });
            }
            continue;
        }

        if head.eq_ignore_ascii_case("reload_quantum_cycles") {
            let q_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "reload_quantum_cycles requires an integer >= 1".into(),
            })?;
            let q: u64 = q_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid reload_quantum_cycles: {q_str}"),
            })?;
            if q < 1 {
                return Err(ParseError {
                    line: line_no,
                    message: "reload_quantum_cycles must be >= 1".into(),
                });
            }
            reload_quantum_cycles = Some(q);
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after reload_quantum_cycles".into(),
                });
            }
            continue;
        }
        if head.eq_ignore_ascii_case("quantum") {
            let q_str = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "quantum requires an integer >= 1".into(),
            })?;
            let q: u64 = q_str.parse().map_err(|_| ParseError {
                line: line_no,
                message: format!("invalid quantum: {q_str}"),
            })?;
            if q < 1 {
                return Err(ParseError {
                    line: line_no,
                    message: "quantum must be >= 1".into(),
                });
            }
            reload_quantum_cycles = Some(q);
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after quantum".into(),
                });
            }
            continue;
        }

        if head.eq_ignore_ascii_case("alias") {
            let name = parts
                .next()
                .ok_or_else(|| ParseError {
                    line: line_no,
                    message: "alias requires <name> <path|bank:index>".into(),
                })?
                .to_string();
            let tok = parts.next().ok_or_else(|| ParseError {
                line: line_no,
                message: "alias requires a sample reference".into(),
            })?;
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "alias: only one reference token allowed in v0.1 (no spaces in path)"
                        .into(),
                });
            }
            let spec =
                parse_sample_token_raw(tok, Some(sample_root)).map_err(|msg| ParseError {
                    line: line_no,
                    message: msg,
                })?;
            aliases.insert(name, spec);
            continue;
        }

        if head.eq_ignore_ascii_case("silence") {
            let name = parts
                .next()
                .ok_or_else(|| ParseError {
                    line: line_no,
                    message: "silence requires a track/orbit name (e.g. silence d1)".into(),
                })?
                .to_string();
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after silence <name>".into(),
                });
            }
            silenced.insert(name);
            continue;
        }

        if head.eq_ignore_ascii_case("mute") {
            let name = parts
                .next()
                .ok_or_else(|| ParseError {
                    line: line_no,
                    message: "mute requires a track/orbit name (e.g. mute d1)".into(),
                })?
                .to_string();
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after mute <name>".into(),
                });
            }
            if name.eq_ignore_ascii_case("all") {
                mute_all = true;
            } else {
                silenced.insert(name);
            }
            continue;
        }

        if head.eq_ignore_ascii_case("unsilence") {
            let name = parts
                .next()
                .ok_or_else(|| ParseError {
                    line: line_no,
                    message: "unsilence requires a track/orbit name".into(),
                })?
                .to_string();
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after unsilence <name>".into(),
                });
            }
            silenced.remove(&name);
            continue;
        }

        if head.eq_ignore_ascii_case("solo") {
            let name = parts
                .next()
                .ok_or_else(|| ParseError {
                    line: line_no,
                    message: "solo requires a track/orbit name".into(),
                })?
                .to_string();
            if parts.next().is_some() {
                return Err(ParseError {
                    line: line_no,
                    message: "extra tokens after solo <name>".into(),
                });
            }
            soloed.insert(name);
            continue;
        }

        if head.eq_ignore_ascii_case("track") {
            let pending = parse_track_line(&line, line_no, &aliases, sample_root, &lets)?;
            pending_tracks.push(pending);
            continue;
        }

        if head.eq_ignore_ascii_case("pat") {
            return Err(ParseError {
                line: line_no,
                message: "use `s \"…\"` (Tidal `sound`), not `pat`".into(),
            });
        }

        return Err(ParseError {
            line: line_no,
            message: format!("unknown directive: {head}"),
        });
    }

    // Tidal-like stream semantics: later definitions of the same orbit/track name
    // replace earlier ones ("last assignment wins").
    let mut tracks = Vec::new();
    let mut track_pos: HashMap<String, usize> = HashMap::new();
    for pending in pending_tracks {
        let PendingTrack::Pattern {
            line_no,
            name,
            gain,
            ast,
            gain_lane,
            pan_lane,
            prob_lane,
            dur_lane,
            legato_lane,
            cut_group_lane,
            cut_voices_lane,
            cutoff_lane,
            tempo,
        } = pending;
        let track = Track {
            name: name.clone(),
            gain,
            kind: TrackKind::Pattern {
                ast,
                gain_lane,
                pan_lane,
                prob_lane,
                dur_lane,
                legato_lane,
                cut_group_lane,
                cut_voices_lane,
                cutoff_lane,
                tempo,
                line: line_no,
            },
        };
        if let Some(&idx) = track_pos.get(&name) {
            tracks[idx] = track;
        } else {
            track_pos.insert(name, tracks.len());
            tracks.push(track);
        }
    }

    Ok(Program {
        bpm,
        master_gain,
        swing,
        humanize_ms,
        reload_quantum_cycles,
        mute_all,
        tracks,
        aliases,
        silenced,
        soloed,
    })
}

/// Resolve a sample path: absolute paths stay as-is; relative paths join `base`.
pub fn resolve_sample_path(base: &Path, track_path: &Path) -> PathBuf {
    if track_path.is_absolute() {
        track_path.to_path_buf()
    } else {
        base.join(track_path)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn parses_example() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = "bpm 120\nd1 $ s \"k.wav k.wav\"\n";
        File::create(root.join("k.wav")).unwrap();
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.bpm, 120);
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].gain, 1.0);
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
        assert!((ev[0].0 - 0.0).abs() < 1e-9);
        assert!((ev[1].0 - 0.5).abs() < 1e-9);
        assert!(ev[0].1.ends_with("k.wav"));
    }

    #[test]
    fn parses_two_tracks_same_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("k.wav")).unwrap();
        File::create(root.join("s.wav")).unwrap();
        let src = "bpm 280\nd1 $ s \"k.wav\"\nd2 $ s \"s.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.bpm, 280);
        assert_eq!(p.tracks.len(), 2);
        let ev0 = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        let ev1 = p.events_for_track(&p.tracks[1], 0, root).unwrap();
        assert_eq!(ev0.len(), 1);
        assert_eq!(ev1.len(), 1);
        assert!((ev0[0].0 - 0.0).abs() < 1e-9);
        assert!((ev1[0].0 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn later_same_orbit_replaces_earlier_definition() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("bd.wav")).unwrap();
        File::create(root.join("sn.wav")).unwrap();
        let src = "d1 $ s \"bd.wav\"\nd1 $ s \"sn.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].name, "d1");
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1.file_name().unwrap(), "sn.wav");
    }

    #[test]
    fn bank_ref_second_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bd = root.join("bd");
        std::fs::create_dir_all(&bd).unwrap();
        File::create(bd.join("a.wav")).unwrap();
        File::create(bd.join("b.wav")).unwrap();
        File::create(bd.join("c.wav")).unwrap();
        let src = "d1 $ s \"bd:2\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev[0].1.file_name().unwrap(), "b.wav");
    }

    #[test]
    fn alias_expands() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bd = root.join("bd");
        std::fs::create_dir_all(&bd).unwrap();
        File::create(bd.join("x.wav")).unwrap();
        let src = "alias foo bd:1\nbpm 100\nd1 $ s \"foo\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev[0].1.file_name().unwrap(), "x.wav");
    }

    #[test]
    fn track_optional_gain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        let src = "d1 $ gain 0.25 s \"a.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks[0].gain, 0.25);
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 1);
        assert!((ev[0].2 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn master_gain_scales_after_track_gain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        let src = "master 0.5\nd1 $ gain 0.4 s \"a.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert!((p.master_gain - 0.5).abs() < 1e-6);
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert!((ev[0].2 - 0.2).abs() < 1e-5);
    }

    #[test]
    fn file_level_performance_controls_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        let src = "swing 0.3\nhumanize_ms 4.5\nreload_quantum_cycles 3\nd1 $ s \"a.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.swing, Some(0.3));
        assert_eq!(p.humanize_ms, Some(4.5));
        assert_eq!(p.reload_quantum_cycles, Some(3));
    }

    #[test]
    fn track_pat_mini() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("k.wav")).unwrap();
        File::create(root.join("s.wav")).unwrap();
        let src = "d1 $ s \"k.wav s.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        let TrackKind::Pattern { .. } = &p.tracks[0].kind;
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
        assert!((ev[0].0 - 0.0).abs() < 1e-9);
        assert!((ev[1].0 - 0.5).abs() < 1e-9);
        assert!((ev[0].2 - 1.0).abs() < 1e-6 && (ev[1].2 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bank_folder_name_defaults_to_first_wav() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sn = root.join("sn");
        std::fs::create_dir_all(&sn).unwrap();
        File::create(sn.join("a.wav")).unwrap();
        File::create(sn.join("b.wav")).unwrap();
        let src = "d1 $ s \"sn\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev[0].1.file_name().unwrap(), "a.wav");
    }

    #[test]
    fn pat_hash_gain_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ s \"a.wav b.wav\" # gain \"1 0.25\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
        assert!((ev[0].2 - 1.0).abs() < 1e-6);
        assert!((ev[1].2 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn pat_hash_pan_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ s \"a.wav b.wav\" # pan \"-1 1\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
        assert!((ev[0].3 + 1.0).abs() < 1e-6);
        assert!((ev[1].3 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unquoted_gain_and_pan_atoms_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        let src = "d1 $ s \"a.wav\" # gain 0.5 # pan -0.25\n";
        let err = parse(src, 100, root).unwrap_err();
        assert!(err.message.contains("quoted pattern"));
    }

    #[test]
    fn prob_lane_requires_quoted_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ s \"a.wav b.wav\" # prob \"1 0.25\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev1 = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev1.len(), 2);
        assert_eq!(ev1[0].4, Some(1.0));
        assert_eq!(ev1[1].4, Some(0.25));

        let bad = "d1 $ s \"a.wav\" # prob 0.5\n";
        let err = parse(bad, 100, root).unwrap_err();
        assert!(err.message.contains("quoted pattern"));
    }

    #[test]
    fn strip_comment_uses_double_dash() {
        let line = r#"d1 $ s "bd sn" # gain "1 0.5"  -- endcomment"#;
        let s = strip_comment(line);
        assert!(s.contains("# gain"));
        assert!(!s.contains("endcomment"));
    }

    #[test]
    fn tidal_s_alias_and_d1() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("k.wav")).unwrap();
        let src = "d1 $ s \"k.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].name, "d1");
        let TrackKind::Pattern { .. } = &p.tracks[0].kind;
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn rev_prefix_outside_sound_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ rev $ s \"a.wav b.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        let mut ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        ev.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1.file_name().unwrap(), "b.wav");
        assert_eq!(ev[1].1.file_name().unwrap(), "a.wav");
    }

    #[test]
    fn pal_and_rot_prefixes_outside_sound_are_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        File::create(root.join("c.wav")).unwrap();
        let src = "d1 $ pal $ rot 1 $ s \"a.wav b.wav c.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert!(ev.len() >= 3);
    }

    #[test]
    fn every_prefix_outside_sound_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ every 2 pal $ s \"a.wav b.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev0 = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        let ev1 = p.events_for_track(&p.tracks[0], 1, root).unwrap();
        assert!(ev0.len() >= 2);
        assert_eq!(ev1.len(), 2);
    }


    #[test]
    fn tidal_d2_prefix_second_orbit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d2 $ s \"a.wav\"\nd1 $ s \"b.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[0].name, "d2");
        assert_eq!(p.tracks[1].name, "d1");
    }

    #[test]
    fn pat_string_may_contain_stack_keyword() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ s \"stack [ a.wav, b.wav ]\"\n";
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn merge_statement_lines_spans_unclosed_quote() {
        // Physical newline becomes a space in the merged line — use two mini tokens.
        let src = "d1 $ s \"bd:1\nsn:1\"\n";
        let m = merge_statement_lines(src);
        assert_eq!(m.len(), 1);
        assert!(m[0].1.contains("bd:1") && m[0].1.contains("sn:1"));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bd = root.join("bd");
        let sn = root.join("sn");
        std::fs::create_dir_all(&bd).unwrap();
        std::fs::create_dir_all(&sn).unwrap();
        File::create(bd.join("a.wav")).unwrap();
        File::create(sn.join("a.wav")).unwrap();
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].name, "d1");
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn silence_mutes_named_track_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ s \"a.wav\"\nd2 $ s \"b.wav\"\nsilence d1\n";
        let p = parse(src, 100, root).unwrap();
        assert!(p.is_silenced("d1"));
        assert!(!p.is_silenced("d2"));
        assert!(p
            .events_for_track(&p.tracks[0], 0, root)
            .unwrap()
            .is_empty());
        assert_eq!(p.events_for_track(&p.tracks[1], 0, root).unwrap().len(), 1);
    }

    #[test]
    fn mute_and_solo_controls_filter_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "mute d1\nd1 $ s \"a.wav\"\nd2 $ s \"b.wav\"\nsolo d2\n";
        let p = parse(src, 100, root).unwrap();
        assert!(p.is_silenced("d1"));
        assert!(p.is_soloed("d2"));
        assert!(p.events_for_track(&p.tracks[0], 0, root).unwrap().is_empty());
        assert_eq!(p.events_for_track(&p.tracks[1], 0, root).unwrap().len(), 1);
    }

    #[test]
    fn unsilence_restores_track() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("k.wav")).unwrap();
        let src = "silence d1\nd1 $ s \"k.wav\"\nunsilence d1\n";
        let p = parse(src, 100, root).unwrap();
        assert!(!p.is_silenced("d1"));
        assert_eq!(p.events_for_track(&p.tracks[0], 0, root).unwrap().len(), 1);
    }

    #[test]
    fn silence_before_track_definition_still_mutes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("x.wav")).unwrap();
        let src = "silence d3\nd3 $ s \"x.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert!(p.is_silenced("d3"));
        assert!(p
            .events_for_track(&p.tracks[0], 0, root)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn slow_two_stretches_two_step_pattern_across_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bd = root.join("bd");
        std::fs::create_dir_all(&bd).unwrap();
        File::create(bd.join("a.wav")).unwrap();
        File::create(bd.join("b.wav")).unwrap();
        let src = "d1 $ slow 2 $ s \"bd:1 bd:2\"\n";
        let p = parse(src, 100, root).unwrap();
        let TrackKind::Pattern { tempo, .. } = &p.tracks[0].kind;
        assert_eq!(tempo, &Some(TempoWrap::Slow(2)));
        let ev0 = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        let ev1 = p.events_for_track(&p.tracks[0], 1, root).unwrap();
        assert_eq!(ev0.len(), 1);
        assert_eq!(ev1.len(), 1);
        assert!((ev0[0].0 - 0.0).abs() < 1e-9);
        assert!((ev1[0].0 - 0.0).abs() < 1e-9);
        assert!(ev0[0].1.file_name().unwrap() == "a.wav");
        assert!(ev1[0].1.file_name().unwrap() == "b.wav");
    }

    #[test]
    fn fast_two_fits_two_inner_cycles_in_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bd = root.join("bd");
        std::fs::create_dir_all(&bd).unwrap();
        File::create(bd.join("a.wav")).unwrap();
        File::create(bd.join("b.wav")).unwrap();
        let src = "d1 $ fast 2 $ s \"bd:1 bd:2\"\n";
        let p = parse(src, 100, root).unwrap();
        let TrackKind::Pattern { tempo, .. } = &p.tracks[0].kind;
        assert_eq!(tempo, &Some(TempoWrap::Fast(2)));
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 4);
        let mut phases: Vec<f64> = ev.iter().map(|e| e.0).collect();
        phases.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((phases[0] - 0.0).abs() < 1e-9);
        assert!((phases[1] - 0.25).abs() < 1e-9);
        assert!((phases[2] - 0.5).abs() < 1e-9);
        assert!((phases[3] - 0.75).abs() < 1e-9);
    }

    #[test]
    fn implicit_d1_accepts_slow_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("x.wav")).unwrap();
        let src = "slow 2 $ s \"x.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        assert_eq!(p.tracks[0].name, "d1");
        let TrackKind::Pattern { tempo, .. } = &p.tracks[0].kind;
        assert_eq!(tempo, &Some(TempoWrap::Slow(2)));
    }

    #[test]
    fn examples_showcase_seq_parses_and_schedules() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        for bank in ["bd", "hh", "sn"] {
            let d = base.join(bank);
            std::fs::create_dir_all(&d).unwrap();
            for i in 1..=16 {
                File::create(d.join(format!("{i:02}.wav"))).unwrap();
            }
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/showcase.seq");
        let src = std::fs::read_to_string(path).unwrap();
        let prog = parse(&src, 100, base).unwrap();
        assert!((1..=400).contains(&prog.bpm));
        assert!(prog.master_gain.is_finite() && prog.master_gain >= 0.0);
        assert!(!prog.tracks.is_empty());
        for t in &prog.tracks {
            prog.events_for_track(t, 0, base).unwrap();
            prog.events_for_track(t, 99, base).unwrap();
        }
    }

    #[test]
    fn merge_statement_lines_bracket_depth_outside_quotes() {
        let src = "x [ a\nb ]\n";
        let m = merge_statement_lines(src);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].1, "x [ a b ]");
    }

    #[test]
    fn rejects_pat_keyword() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        let err = parse("d1 $ pat \"a.wav\"\n", 100, root).unwrap_err();
        assert!(err.message.contains("s"));
    }

    #[test]
    fn nested_rev_pal_chain_outer_rev_wraps_inner_pal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = "d1 $ rev $ pal $ s \"a.wav b.wav\"\n";
        let p = parse(src, 100, root).unwrap();
        let mut ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        ev.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1.file_name().unwrap(), "b.wav");
        assert_eq!(ev[1].1.file_name().unwrap(), "a.wav");
    }

    #[test]
    fn let_expands_in_sound_and_hash_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("a.wav")).unwrap();
        File::create(root.join("b.wav")).unwrap();
        let src = concat!(
            "let hits \"a.wav b.wav\"\n",
            "let gains \"1 0.25\"\n",
            "d1 $ s \"hits\" # gain \"gains\"\n",
        );
        let p = parse(src, 100, root).unwrap();
        let ev = p.events_for_track(&p.tracks[0], 0, root).unwrap();
        assert_eq!(ev.len(), 2);
        assert!((ev[0].2 - 1.0).abs() < 1e-6);
        assert!((ev[1].2 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn include_inlines_relative_file_next_to_base() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("k.wav")).unwrap();
        std::fs::write(
            tmp.path().join("part.seq"),
            "d1 $ s \"k.wav\"\n",
        )
        .unwrap();
        let main_src = "include \"part.seq\"\n";
        let base = tmp.path().join("main.seq");
        std::fs::write(&base, main_src).unwrap();
        let base_dir = base.parent().unwrap();
        let p = parse_with_options(main_src, 100, root, Some(base_dir)).unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.events_for_track(&p.tracks[0], 0, root).unwrap().len(), 1);
    }

    #[test]
    fn include_cycle_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        File::create(root.join("x.wav")).unwrap();
        std::fs::write(tmp.path().join("a.seq"), "include \"b.seq\"\n").unwrap();
        std::fs::write(tmp.path().join("b.seq"), "include \"a.seq\"\n").unwrap();
        let base = tmp.path().join("entry.seq");
        std::fs::write(&base, "include \"a.seq\"\n").unwrap();
        let err = parse_with_options(
            "include \"a.seq\"\n",
            100,
            root,
            Some(base.parent().unwrap()),
        )
        .unwrap_err();
        assert!(err.message.contains("cycle"), "{}", err.message);
    }
}
