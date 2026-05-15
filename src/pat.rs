//! Tidal-inspired mini-notation: `[]` stack, `<>` alternate, spaces = sequence, `~` = rest,
//! `stack [a, b]` parallel layers, `bd*3` repeat.

#[derive(Debug, Clone, PartialEq)]
pub enum Pat {
    Seq(Vec<Pat>),
    Stack(Vec<Pat>),
    /// Tidal `stack [ p , q ]`: full-cycle patterns layered (all of `[t0,t1)`).
    Layer(Vec<Pat>),
    Alt(Vec<Pat>),
    Rev(Box<Pat>),
    Rot(i32, Box<Pat>),
    Pal(Box<Pat>),
    Every(u32, Box<Pat>, Box<Pat>),
    Fast(u32, Box<Pat>),
    Slow(u32, Box<Pat>),
    Euclid(u32, u32, u32, String),
    Atom(String),
    Rest,
}

/// Parse a full pattern string (contents inside quotes, no surrounding `"`).
pub fn parse_pattern(source: &str) -> Result<Pat, String> {
    let mut i = 0usize;
    let items = parse_seq(source, &mut i, None)?;
    skip_ws(source, &mut i);
    if i != source.len() {
        return Err(format!(
            "unexpected trailing input at byte {i}: {:?}",
            &source[i..source.len().min(i + 20)]
        ));
    }
    Ok(if items.len() == 1 {
        items.into_iter().next().unwrap()
    } else {
        Pat::Seq(items)
    })
}

fn skip_ws(s: &str, i: &mut usize) {
    let b = s.as_bytes();
    while *i < b.len() && b[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn parse_seq(s: &str, i: &mut usize, end: Option<u8>) -> Result<Vec<Pat>, String> {
    let mut out = Vec::new();
    loop {
        skip_ws(s, i);
        if *i >= s.len() {
            break;
        }
        let b = s.as_bytes()[*i];
        if Some(b) == end {
            break;
        }
        out.push(parse_item(s, i, end)?);
    }
    Ok(out)
}

fn parse_item(s: &str, i: &mut usize, _end: Option<u8>) -> Result<Pat, String> {
    skip_ws(s, i);
    if *i >= s.len() {
        return Err("unexpected end of pattern".into());
    }
    if starts_with_keyword(s, *i, "stack") {
        return parse_tidal_stack(s, i);
    }
    if starts_with_keyword(s, *i, "euclid") {
        return parse_euclid(s, i);
    }
    let bytes = s.as_bytes();
    match bytes[*i] {
        b'[' => {
            *i += 1;
            let inner = parse_seq(s, i, Some(b']'))?;
            skip_ws(s, i);
            if *i >= s.len() || bytes.get(*i) != Some(&b']') {
                return Err("unclosed '['".into());
            }
            *i += 1;
            if inner.is_empty() {
                return Err("empty stack '[]'".into());
            }
            Ok(Pat::Stack(inner))
        }
        b'<' => {
            *i += 1;
            let inner = parse_seq(s, i, Some(b'>'))?;
            skip_ws(s, i);
            if *i >= s.len() || bytes.get(*i) != Some(&b'>') {
                return Err("unclosed '<'".into());
            }
            *i += 1;
            if inner.is_empty() {
                return Err("empty alternate '<>'".into());
            }
            Ok(Pat::Alt(inner))
        }
        b'~' => {
            *i += 1;
            Ok(Pat::Rest)
        }
        _ => {
            let start = *i;
            while *i < bytes.len() {
                let c = bytes[*i];
                if c.is_ascii_whitespace() || c == b'[' || c == b']' || c == b'<' || c == b'>' {
                    break;
                }
                *i += 1;
            }
            if start == *i {
                return Err("expected atom".into());
            }
            let tok = s[start..*i].to_string();
            if tok.is_empty() {
                return Err("empty atom".into());
            }
            if ["rev", "pal", "rot", "every", "fast", "slow"]
                .iter()
                .any(|kw| tok.eq_ignore_ascii_case(kw))
            {
                return Err(format!(
                    "transform `{tok}` must be used outside quotes (e.g. `{tok} $ s \"...\"`)"
                ));
            }
            expand_star_atom(tok)
        }
    }
}

fn parse_int_word(s: &str, i: &mut usize, what: &str) -> Result<i32, String> {
    skip_ws(s, i);
    let start = *i;
    let b = s.as_bytes();
    if *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
        *i += 1;
    }
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i == start || (start + 1 == *i && (b[start] == b'+' || b[start] == b'-')) {
        return Err(format!("{what}: expected integer argument"));
    }
    s[start..*i]
        .parse::<i32>()
        .map_err(|_| format!("{what}: invalid integer"))
}

fn parse_u32_word(s: &str, i: &mut usize, what: &str) -> Result<u32, String> {
    let v = parse_int_word(s, i, what)?;
    if v < 0 {
        return Err(format!("{what}: expected non-negative integer"));
    }
    u32::try_from(v).map_err(|_| format!("{what}: integer out of range"))
}

fn parse_word_token(s: &str, i: &mut usize) -> Option<String> {
    skip_ws(s, i);
    let b = s.as_bytes();
    let start = *i;
    while *i < b.len() && !b[*i].is_ascii_whitespace() && !b"[]<>".contains(&b[*i]) {
        *i += 1;
    }
    if *i == start {
        None
    } else {
        Some(s[start..*i].to_ascii_lowercase())
    }
}

fn parse_euclid(s: &str, i: &mut usize) -> Result<Pat, String> {
    *i += 6;
    skip_ws(s, i);
    let b = s.as_bytes();
    if *i >= b.len() || b[*i] != b'(' {
        return Err("euclid: expected `(`".into());
    }
    *i += 1;
    let k = parse_u32_word(s, i, "euclid")?;
    skip_ws(s, i);
    if *i >= b.len() || b[*i] != b',' {
        return Err("euclid: expected comma after pulses".into());
    }
    *i += 1;
    let n = parse_u32_word(s, i, "euclid")?;
    skip_ws(s, i);
    let mut rot = 0u32;
    if *i < b.len() && b[*i] == b',' {
        *i += 1;
        rot = parse_u32_word(s, i, "euclid")?;
        skip_ws(s, i);
    }
    if *i >= b.len() || b[*i] != b')' {
        return Err("euclid: expected `)`".into());
    }
    *i += 1;
    if n < 1 {
        return Err("euclid: steps must be >= 1".into());
    }
    if k > n {
        return Err("euclid: pulses must be <= steps".into());
    }
    let sample = parse_word_token(s, i).ok_or_else(|| "euclid: expected sample token".to_string())?;
    Ok(Pat::Euclid(k, n, rot, sample))
}

fn starts_with_keyword(s: &str, i: usize, kw: &str) -> bool {
    let b = s.as_bytes();
    if i + kw.len() > b.len() {
        return false;
    }
    if !s[i..i + kw.len()].eq_ignore_ascii_case(kw) {
        return false;
    }
    let after = i + kw.len();
    after >= s.len() || !b[after].is_ascii_alphanumeric() && b[after] != b'_'
}

fn find_close_bracket(s: &str, inner_start: usize) -> Result<usize, String> {
    let b = s.as_bytes();
    let mut depth = 1i32;
    let mut idx = inner_start;
    let mut in_quote = false;
    while idx < b.len() {
        match b[idx] {
            b'"' => {
                in_quote = !in_quote;
                idx += 1;
            }
            b'[' if !in_quote => {
                depth += 1;
                idx += 1;
            }
            b']' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    return Ok(idx);
                }
                idx += 1;
            }
            _ => idx += 1,
        }
    }
    Err("unclosed `[`".into())
}

fn parse_tidal_stack(s: &str, i: &mut usize) -> Result<Pat, String> {
    if !starts_with_keyword(s, *i, "stack") {
        return Err("internal: expected `stack`".into());
    }
    *i += 5;
    skip_ws(s, i);
    let b = s.as_bytes();
    if *i >= b.len() || b[*i] != b'[' {
        return Err("`stack` must be followed by `[`".into());
    }
    *i += 1;
    let body_start = *i;
    let body_end = find_close_bracket(s, body_start)?;
    *i = body_end + 1;
    let body = &s[body_start..body_end];
    let parts = split_comma_top_level(body)?;
    if parts.is_empty() {
        return Err("empty `stack []`".into());
    }
    let mut layers = Vec::with_capacity(parts.len());
    for p in parts {
        layers.push(parse_pattern(p)?);
    }
    Ok(Pat::Layer(layers))
}

fn split_comma_top_level(body: &str) -> Result<Vec<&str>, String> {
    let mut out: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut start = 0usize;
    let b = body.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' => {
                in_quote = !in_quote;
                i += 1;
            }
            b'[' | b'<' if !in_quote => {
                depth += 1;
                i += 1;
            }
            b']' | b'>' if !in_quote => {
                depth -= 1;
                i += 1;
            }
            b',' if !in_quote && depth == 0 => {
                let chunk = body[start..i].trim();
                if !chunk.is_empty() {
                    out.push(chunk);
                }
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let chunk = body[start..].trim();
    if !chunk.is_empty() {
        out.push(chunk);
    }
    if out.is_empty() {
        return Err("nothing between commas in stack".into());
    }
    Ok(out)
}

/// `bd*3` → three sequential steps; `bd*1` → one step. Plain tokens unchanged.
fn expand_star_atom(tok: String) -> Result<Pat, String> {
    if tok.is_empty() {
        return Err("empty atom".into());
    }
    if let Some(pos) = tok.rfind('*') {
        let head = &tok[..pos];
        let tail = &tok[pos + 1..];
        if !head.is_empty()
            && !tail.is_empty()
            && tail.chars().all(|c| c.is_ascii_digit())
            && head.chars().all(|c| !c.is_ascii_whitespace())
        {
            let n: usize = tail.parse().map_err(|_| "bad * repeat count".to_string())?;
            if !(1..=256).contains(&n) {
                return Err(format!("repeat count must be 1–256, got {n}"));
            }
            let base = head.to_string();
            return Ok(Pat::Seq((0..n).map(|_| Pat::Atom(base.clone())).collect()));
        }
    }
    Ok(Pat::Atom(tok))
}

/// Events: sample token + onset in [t0,t1) relative to cycle (typically t0=0,t1=1).
pub fn eval(pat: &Pat, cycle: u64, t0: f64, t1: f64) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    eval_inner(pat, cycle, t0, t1, &mut out);
    out
}

/// Sound hits + per-hit gain multiplier (before track `gain`).
///
/// Tidal-style **`#` gain**: the gain mini-pattern is flattened over the cycle (same time
/// subdivision as sound: seq / stack / `<>`) into a list of multipliers; that list **repeats**
/// across sound hits (`hit_index % len`). A single value like `"0.4"` applies to every hit.
/// `~` in the gain lane contributes **1.0** so the rhythmic period still advances.
pub fn eval_with_gains(
    sound: &Pat,
    gain: Option<&Pat>,
    cycle: u64,
    t0: f64,
    t1: f64,
) -> Result<Vec<(f64, String, f32)>, String> {
    let hits = eval(sound, cycle, t0, t1);
    match gain {
        None => Ok(hits.into_iter().map(|(t, s)| (t, s, 1.0f32)).collect()),
        Some(g) => {
            let muls = flatten_gain_muls(g, cycle, t0, t1)?;
            let table = if muls.is_empty() { vec![1.0f32] } else { muls };
            Ok(hits
                .into_iter()
                .enumerate()
                .map(|(i, (t, s))| {
                    let m = table[i % table.len()];
                    (t, s, m)
                })
                .collect())
        }
    }
}

/// Sound hits + per-hit gain and pan values.
///
/// - gain lane defaults to 1.0 (and `~` is neutral 1.0)
/// - pan lane defaults to 0.0 center (and `~` is neutral 0.0)
pub fn eval_with_gain_pan(
    sound: &Pat,
    gain: Option<&Pat>,
    pan: Option<&Pat>,
    cycle: u64,
    t0: f64,
    t1: f64,
) -> Result<Vec<(f64, String, f32, f32)>, String> {
    let hits = eval(sound, cycle, t0, t1);
    let gain_table = match gain {
        None => vec![1.0f32],
        Some(g) => {
            let muls = flatten_gain_muls(g, cycle, t0, t1)?;
            if muls.is_empty() { vec![1.0] } else { muls }
        }
    };
    let pan_table = match pan {
        None => vec![0.0f32],
        Some(p) => {
            let vals = flatten_pan_vals(p, cycle, t0, t1)?;
            if vals.is_empty() { vec![0.0] } else { vals }
        }
    };
    Ok(hits
        .into_iter()
        .enumerate()
        .map(|(i, (t, s))| {
            let g = gain_table[i % gain_table.len()];
            let p = pan_table[i % pan_table.len()];
            (t, s, g, p)
        })
        .collect())
}

/// Sound hits with all currently-supported per-hit control lanes.
#[allow(clippy::type_complexity)]
pub fn eval_with_control_lanes(
    sound: &Pat,
    gain: Option<&Pat>,
    pan: Option<&Pat>,
    prob: Option<&Pat>,
    dur: Option<&Pat>,
    legato: Option<&Pat>,
    cut_group: Option<&Pat>,
    cut_voices: Option<&Pat>,
    cutoff: Option<&Pat>,
    cycle: u64,
    t0: f64,
    t1: f64,
) -> Result<Vec<(f64, String, f32, f32, Option<f32>, Option<f32>, Option<f32>, Option<i32>, Option<u32>, Option<f32>)>, String>
{
    let hits = eval(sound, cycle, t0, t1);
    let gain_table = match gain {
        None => vec![1.0f32],
        Some(g) => {
            let muls = flatten_gain_muls(g, cycle, t0, t1)?;
            if muls.is_empty() { vec![1.0] } else { muls }
        }
    };
    let pan_table = match pan {
        None => vec![0.0f32],
        Some(p) => {
            let vals = flatten_pan_vals(p, cycle, t0, t1)?;
            if vals.is_empty() { vec![0.0] } else { vals }
        }
    };
    let prob_table = match prob {
        None => vec![Some(1.0f32)],
        Some(p) => {
            let vals = flatten_lane_opt_vals(p, cycle, t0, t1, parse_prob_atom)?;
            if vals.is_empty() { vec![Some(1.0)] } else { vals }
        }
    };
    let dur_table = match dur {
        None => vec![None],
        Some(d) => {
            let vals = flatten_lane_opt_vals(d, cycle, t0, t1, parse_dur_ms_atom)?;
            if vals.is_empty() { vec![None] } else { vals }
        }
    };
    let legato_table = match legato {
        None => vec![None],
        Some(l) => {
            let vals = flatten_lane_opt_vals(l, cycle, t0, t1, parse_legato_atom)?;
            if vals.is_empty() { vec![None] } else { vals }
        }
    };
    let cut_group_table = match cut_group {
        None => vec![None],
        Some(c) => {
            let vals = flatten_lane_opt_vals(c, cycle, t0, t1, parse_cut_group_atom)?;
            if vals.is_empty() { vec![None] } else { vals }
        }
    };
    let cut_voices_table = match cut_voices {
        None => vec![None],
        Some(c) => {
            let vals = flatten_lane_opt_vals(c, cycle, t0, t1, parse_cut_voices_atom)?;
            if vals.is_empty() { vec![None] } else { vals }
        }
    };
    let cutoff_table = match cutoff {
        None => vec![None],
        Some(c) => {
            let vals = flatten_lane_opt_vals(c, cycle, t0, t1, parse_cutoff_atom)?;
            if vals.is_empty() { vec![None] } else { vals }
        }
    };
    Ok(hits
        .into_iter()
        .enumerate()
        .map(|(i, (t, s))| {
            (
                t,
                s,
                gain_table[i % gain_table.len()],
                pan_table[i % pan_table.len()],
                prob_table[i % prob_table.len()],
                dur_table[i % dur_table.len()],
                legato_table[i % legato_table.len()],
                cut_group_table[i % cut_group_table.len()],
                cut_voices_table[i % cut_voices_table.len()],
                cutoff_table[i % cutoff_table.len()],
            )
        })
        .collect())
}

fn parse_gain_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("gain lane: expected a non-negative number, got {atom:?}"))?;
    if !x.is_finite() || x < 0.0 {
        return Err(format!("gain lane: invalid value {x}"));
    }
    Ok(x)
}

fn parse_pan_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("pan lane: expected a number in [-1,1], got {atom:?}"))?;
    if !x.is_finite() || !(-1.0..=1.0).contains(&x) {
        return Err(format!("pan lane: invalid value {x} (expected -1..=1)"));
    }
    Ok(x)
}

fn parse_prob_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("prob lane: expected a number in [0,1], got {atom:?}"))?;
    if !x.is_finite() || !(0.0..=1.0).contains(&x) {
        return Err(format!("prob lane: invalid value {x} (expected 0..=1)"));
    }
    Ok(x)
}

fn parse_dur_ms_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("dur lane: expected a non-negative number (ms), got {atom:?}"))?;
    if !x.is_finite() || x < 0.0 {
        return Err(format!("dur lane: invalid value {x}"));
    }
    Ok(x)
}

fn parse_legato_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("legato lane: expected a non-negative number, got {atom:?}"))?;
    if !x.is_finite() || x < 0.0 {
        return Err(format!("legato lane: invalid value {x}"));
    }
    Ok(x)
}

fn parse_cut_group_atom(atom: &str) -> Result<i32, String> {
    let x: i32 = atom
        .parse()
        .map_err(|_| format!("cut lane: expected integer group >= 0, got {atom:?}"))?;
    if x < 0 {
        return Err(format!("cut lane: invalid group {x}"));
    }
    Ok(x)
}

fn parse_cut_voices_atom(atom: &str) -> Result<u32, String> {
    let x: u32 = atom
        .parse()
        .map_err(|_| format!("cut lane: expected integer voices >= 1, got {atom:?}"))?;
    if x < 1 {
        return Err("cut lane: voices must be >= 1".into());
    }
    Ok(x)
}

fn parse_cutoff_atom(atom: &str) -> Result<f32, String> {
    let x: f32 = atom
        .parse()
        .map_err(|_| format!("cutoff lane: expected non-negative milliseconds, got {atom:?}"))?;
    if !x.is_finite() || x < 0.0 {
        return Err(format!("cutoff lane: invalid value {x}"));
    }
    Ok(x)
}

/// Flatten gain pattern to multipliers in the **same traversal order** as [`eval_inner`] for sound.
fn flatten_gain_muls(pat: &Pat, cycle: u64, t0: f64, t1: f64) -> Result<Vec<f32>, String> {
    let hits = eval(pat, cycle, t0, t1);
    if hits.is_empty() {
        return Ok(vec![1.0]);
    }
    hits.into_iter()
        .map(|(_, atom)| parse_gain_atom(&atom))
        .collect()
}

/// Flatten pan pattern to values in [-1,1] in the same traversal order as sound.
fn flatten_pan_vals(pat: &Pat, cycle: u64, t0: f64, t1: f64) -> Result<Vec<f32>, String> {
    let hits = eval(pat, cycle, t0, t1);
    if hits.is_empty() {
        return Ok(vec![0.0]);
    }
    hits.into_iter()
        .map(|(_, atom)| parse_pan_atom(&atom))
        .collect()
}

fn flatten_lane_opt_vals<T: Copy>(
    pat: &Pat,
    cycle: u64,
    t0: f64,
    t1: f64,
    parse_atom: fn(&str) -> Result<T, String>,
) -> Result<Vec<Option<T>>, String> {
    let hits = eval(pat, cycle, t0, t1);
    if hits.is_empty() {
        return Ok(vec![None]);
    }
    hits.into_iter()
        .map(|(_, atom)| parse_atom(&atom).map(Some))
        .collect()
}

fn eval_inner(pat: &Pat, cycle: u64, t0: f64, t1: f64, out: &mut Vec<(f64, String)>) {
    match pat {
        Pat::Seq(children) => {
            let n = children.len();
            if n == 0 {
                return;
            }
            let w = (t1 - t0) / n as f64;
            for (k, ch) in children.iter().enumerate() {
                let a = t0 + k as f64 * w;
                let b = t0 + (k + 1) as f64 * w;
                eval_inner(ch, cycle, a, b, out);
            }
        }
        Pat::Stack(children) | Pat::Layer(children) => {
            for ch in children {
                eval_inner(ch, cycle, t0, t1, out);
            }
        }
        Pat::Alt(children) => {
            if children.is_empty() {
                return;
            }
            let k = (cycle as usize) % children.len();
            eval_inner(&children[k], cycle, t0, t1, out);
        }
        Pat::Rev(inner) => {
            let mut tmp = Vec::new();
            eval_inner(inner, cycle, t0, t1, &mut tmp);
            let onsets = sorted_unique_onsets(&tmp);
            if onsets.is_empty() {
                return;
            }
            for (t, tok) in tmp {
                let idx = onset_index(&onsets, t);
                let rev_t = onsets[onsets.len() - 1 - idx];
                out.push((rev_t, tok));
            }
        }
        Pat::Rot(steps, inner) => {
            let mut tmp = Vec::new();
            eval_inner(inner, cycle, t0, t1, &mut tmp);
            let onsets = sorted_unique_onsets(&tmp);
            if onsets.is_empty() {
                return;
            }
            let len = onsets.len() as i32;
            let shift = steps.rem_euclid(len) as usize;
            for (t, tok) in tmp {
                let idx = onset_index(&onsets, t);
                let rot_t = onsets[(idx + shift) % onsets.len()];
                out.push((rot_t, tok));
            }
        }
        Pat::Pal(inner) => {
            let mut tmp = Vec::new();
            eval_inner(inner, cycle, t0, t1, &mut tmp);
            let onsets = sorted_unique_onsets(&tmp);
            if onsets.is_empty() {
                return;
            }
            let mut buckets: Vec<Vec<String>> = vec![Vec::new(); onsets.len()];
            for (t, tok) in tmp {
                let idx = onset_index(&onsets, t);
                buckets[idx].push(tok);
            }
            let mut order: Vec<usize> = (0..onsets.len()).collect();
            if onsets.len() > 1 {
                for j in (1..onsets.len() - 1).rev() {
                    order.push(j);
                }
            }
            let span = t1 - t0;
            let n = order.len().max(1);
            for (j, &bucket_idx) in order.iter().enumerate() {
                let t = t0 + span * (j as f64 / n as f64);
                for tok in &buckets[bucket_idx] {
                    out.push((t, tok.clone()));
                }
            }
        }
        Pat::Every(n, transformed, base) => {
            if *n > 0 && cycle.is_multiple_of(u64::from(*n)) {
                eval_inner(transformed, cycle, t0, t1, out);
            } else {
                eval_inner(base, cycle, t0, t1, out);
            }
        }
        Pat::Fast(n, inner) => {
            let m = (*n).max(1) as f64;
            for j in 0..(*n).max(1) {
                let a = t0 + (j as f64 / m) * (t1 - t0);
                let b = t0 + ((j + 1) as f64 / m) * (t1 - t0);
                eval_inner(inner, cycle.saturating_mul(u64::from((*n).max(1))).saturating_add(u64::from(j)), a, b, out);
            }
        }
        Pat::Slow(n, inner) => {
            let k = (*n).max(1);
            let lc = cycle / u64::from(k);
            let slot = cycle % u64::from(k);
            let mut tmp = Vec::new();
            eval_inner(inner, lc, t0, t1, &mut tmp);
            for (phase, tok) in tmp {
                let rel = ((phase - t0) / (t1 - t0).max(f64::EPSILON)).clamp(0.0, 0.999_999);
                let hit_slot = (rel * f64::from(k)).floor() as u64;
                if hit_slot == slot {
                    let local = (rel * f64::from(k)) % 1.0;
                    out.push((t0 + local * (t1 - t0), tok));
                }
            }
        }
        Pat::Euclid(k, n, r, sample) => {
            for step in 0..*n {
                let idx = (step + *r) % *n;
                if euclid_gate(*k, *n, idx) {
                    let t = t0 + (f64::from(step) / f64::from(*n)) * (t1 - t0);
                    out.push((t, sample.clone()));
                }
            }
        }
        Pat::Atom(s) => {
            out.push((t0, s.clone()));
        }
        Pat::Rest => {}
    }
}

fn euclid_gate(k: u32, n: u32, idx: u32) -> bool {
    if n == 0 || k == 0 {
        return false;
    }
    ((idx * k) % n) < k
}

fn sorted_unique_onsets(events: &[(f64, String)]) -> Vec<f64> {
    let mut onsets: Vec<f64> = events.iter().map(|(t, _)| *t).collect();
    onsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    onsets.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    onsets
}

fn onset_index(onsets: &[f64], t: f64) -> usize {
    onsets
        .iter()
        .position(|x| (*x - t).abs() < 1e-9)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_two_atoms() {
        let p = parse_pattern("bd sn").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 2);
        assert!((e[0].0 - 0.0).abs() < 1e-9 && e[0].1 == "bd");
        assert!((e[1].0 - 0.5).abs() < 1e-9 && e[1].1 == "sn");
    }

    #[test]
    fn stack_same_onset() {
        let p = parse_pattern("[sn hh]").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 2);
        assert!((e[0].0 - 0.0).abs() < 1e-9);
        assert!((e[1].0 - 0.0).abs() < 1e-9);
    }

    #[test]
    fn alternate_cycles() {
        let p = parse_pattern("<bd sn>").unwrap();
        let e0 = eval(&p, 0, 0.0, 1.0);
        let e1 = eval(&p, 1, 0.0, 1.0);
        assert_eq!(e0.len(), 1);
        assert_eq!(e0[0].1, "bd");
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].1, "sn");
    }

    #[test]
    fn hat_bd_sn_bd_thirds() {
        let p = parse_pattern("hat <bd [sn sn]> bd").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        // Cycle 0: middle branch is bd only
        assert_eq!(e.len(), 3);
        assert!((e[0].0 - 0.0).abs() < 1e-6 && e[0].1 == "hat");
        assert!((e[1].0 - 1.0 / 3.0).abs() < 1e-6 && e[1].1 == "bd");
        assert!((e[2].0 - 2.0 / 3.0).abs() < 1e-6 && e[2].1 == "bd");
        let e_odd = eval(&p, 1, 0.0, 1.0);
        // Cycle 1: middle branch is [sn sn] at 1/3, last bd at 2/3
        assert_eq!(e_odd.len(), 4);
        assert!((e_odd[0].0 - 0.0).abs() < 1e-6 && e_odd[0].1 == "hat");
        assert!((e_odd[1].0 - 1.0 / 3.0).abs() < 1e-6 && e_odd[1].1 == "sn");
        assert!((e_odd[2].0 - 1.0 / 3.0).abs() < 1e-6 && e_odd[2].1 == "sn");
        assert!((e_odd[3].0 - 2.0 / 3.0).abs() < 1e-6 && e_odd[3].1 == "bd");
    }

    #[test]
    fn rest_takes_slice() {
        let p = parse_pattern("bd ~ sn").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 2);
        assert!((e[0].0 - 0.0).abs() < 1e-9);
        assert!((e[1].0 - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn gain_lane_zips() {
        let s = parse_pattern("bd sn").unwrap();
        let g = parse_pattern("1 0.5").unwrap();
        let e = eval_with_gains(&s, Some(&g), 0, 0.0, 1.0).unwrap();
        assert_eq!(e.len(), 2);
        assert!((e[0].2 - 1.0).abs() < 1e-6);
        assert!((e[1].2 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn gain_lane_alt_cycles_with_sound() {
        let s = parse_pattern("<bd sn>").unwrap();
        let g = parse_pattern("<1 0.25>").unwrap();
        let e = eval_with_gains(&s, Some(&g), 1, 0.0, 1.0).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].1, "sn");
        assert!((e[0].2 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn gain_shorter_pattern_repeats_across_hits() {
        let s = parse_pattern("bd sn hh cp").unwrap();
        let g = parse_pattern("1 0").unwrap();
        let e = eval_with_gains(&s, Some(&g), 0, 0.0, 1.0).unwrap();
        assert_eq!(e.len(), 4);
        assert!((e[0].2 - 1.0).abs() < 1e-6);
        assert!((e[1].2 - 0.0).abs() < 1e-6);
        assert!((e[2].2 - 1.0).abs() < 1e-6);
        assert!((e[3].2 - 0.0).abs() < 1e-6);
    }

    #[test]
    fn gain_single_atom_applies_to_every_sound_hit() {
        let s = parse_pattern("bd ~ sn hh").unwrap();
        let g = parse_pattern("0.4").unwrap();
        let e = eval_with_gains(&s, Some(&g), 0, 0.0, 1.0).unwrap();
        assert_eq!(e.len(), 3);
        for x in &e {
            assert!((x.2 - 0.4).abs() < 1e-6);
        }
    }

    #[test]
    fn star_repeat_subdivide() {
        let p = parse_pattern("bd*2 sn").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 3);
        assert!((e[0].0 - 0.0).abs() < 1e-6 && e[0].1 == "bd");
        assert!((e[1].0 - 0.25).abs() < 1e-6 && e[1].1 == "bd");
        assert!((e[2].0 - 0.5).abs() < 1e-6 && e[2].1 == "sn");
    }

    #[test]
    fn tidal_stack_layers_full_cycle() {
        let p = parse_pattern("stack [ bd sn, hh cp ]").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 4);
        let mut fs: Vec<f64> = e.iter().map(|x| x.0).collect();
        fs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((fs[0] - 0.0).abs() < 1e-6);
        assert!((fs[1] - 0.0).abs() < 1e-6);
        assert!((fs[2] - 0.5).abs() < 1e-6);
        assert!((fs[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rev_transform_reverses_hit_order() {
        let p = Pat::Rev(Box::new(parse_pattern("bd sn hh").unwrap()));
        let mut e = eval(&p, 0, 0.0, 1.0);
        e.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].1, "hh");
        assert_eq!(e[1].1, "sn");
        assert_eq!(e[2].1, "bd");
    }

    #[test]
    fn rev_preserves_stacked_groups() {
        let p = Pat::Rev(Box::new(parse_pattern("[bd sn] hh").unwrap()));
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 3);
        let mut at_zero: Vec<&str> = e
            .iter()
            .filter(|(t, _)| (*t - 0.0).abs() < 1e-9)
            .map(|(_, s)| s.as_str())
            .collect();
        at_zero.sort();
        assert_eq!(at_zero, vec!["hh"]);
        let mut at_half: Vec<&str> = e
            .iter()
            .filter(|(t, _)| (*t - 0.5).abs() < 1e-9)
            .map(|(_, s)| s.as_str())
            .collect();
        at_half.sort();
        assert_eq!(at_half, vec!["bd", "sn"]);
    }

    #[test]
    fn euclid_generates_expected_pulses() {
        let p = parse_pattern("euclid(3,8,0) bd").unwrap();
        let e = eval(&p, 0, 0.0, 1.0);
        assert_eq!(e.len(), 3);
        assert!((e[0].0 - 0.0).abs() < 1e-9);
        assert!((e[1].0 - 0.375).abs() < 1e-9);
        assert!((e[2].0 - 0.75).abs() < 1e-9);
    }

    #[test]
    fn every_applies_transform_on_matching_cycles() {
        let base = parse_pattern("bd sn").unwrap();
        let p = Pat::Every(
            2,
            Box::new(Pat::Pal(Box::new(base.clone()))),
            Box::new(base),
        );
        let mut e0 = eval(&p, 0, 0.0, 1.0);
        let mut e1 = eval(&p, 1, 0.0, 1.0);
        e0.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        e1.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(e0[0].1, "bd");
        assert_eq!(e1[0].1, "bd");
    }

    #[test]
    fn transform_keywords_are_rejected_inside_pattern_string() {
        let err = parse_pattern("pal bd sn").unwrap_err();
        assert!(err.contains("outside quotes"));
    }
}
