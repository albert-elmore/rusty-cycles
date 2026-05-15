//! Phase 0 golden-style regression tests (see `docs/phase0.md`).
use std::fs::File;
use std::path::Path;

use sequencer::dsl::{parse, TempoWrap, TrackKind};
use sequencer::event_spec::{normalize_schedule, NormalizedHit};

fn fixture_standard_banks(root: &Path) {
    for bank in ["bd", "sn", "hh", "cp"] {
        let d = root.join(bank);
        std::fs::create_dir_all(&d).unwrap();
        for n in ["a.wav", "b.wav", "c.wav"] {
            File::create(d.join(n)).unwrap();
        }
    }
}

fn hit(phase: f64, sample: &str, gain: f32) -> NormalizedHit {
    NormalizedHit {
        phase,
        sample_relpath: sample.into(),
        gain,
        pan: 0.0,
    }
}

#[test]
fn golden_pattern_four_equal_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"bd:1 bd:1 bd:1 bd:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let raw = p.events_for_track(&p.tracks[0], 0, root).unwrap();
    let h = normalize_schedule(root, raw);
    let expected = vec![
        hit(0.0, "bd/a.wav", 1.0),
        hit(0.25, "bd/a.wav", 1.0),
        hit(0.5, "bd/a.wav", 1.0),
        hit(0.75, "bd/a.wav", 1.0),
    ];
    assert_eq!(h, expected);
}

#[test]
fn golden_pat_seq_two_steps_resolves_banks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"bd:1 sn:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let raw = p.events_for_track(&p.tracks[0], 0, root).unwrap();
    let h = normalize_schedule(root, raw);
    assert_eq!(
        h,
        vec![hit(0.0, "bd/a.wav", 1.0), hit(0.5, "sn/a.wav", 1.0),]
    );
}

#[test]
fn golden_alternate_cycles_0_and_1() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"<bd:1 sn:1>\"\n";
    let p = parse(src, 100, root).unwrap();
    let t = &p.tracks[0];
    let h0 = normalize_schedule(root, p.events_for_track(t, 0, root).unwrap());
    let h1 = normalize_schedule(root, p.events_for_track(t, 1, root).unwrap());
    assert_eq!(h0, vec![hit(0.0, "bd/a.wav", 1.0)]);
    assert_eq!(h1, vec![hit(0.0, "sn/a.wav", 1.0)]);
}

#[test]
fn golden_stack_same_step_two_samples() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"[bd:1 sn:1]\"\n";
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(
        h,
        vec![hit(0.0, "bd/a.wav", 1.0), hit(0.0, "sn/a.wav", 1.0),]
    );
}

#[test]
fn golden_stack_layers_full_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"stack [ bd:1 sn:1, hh:1 cp:1 ]\"\n";
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(h.len(), 4);
    assert_eq!(h[0], hit(0.0, "bd/a.wav", 1.0));
    assert_eq!(h[1], hit(0.0, "hh/a.wav", 1.0));
    // Same phase ties broken by sample path (stable sort).
    assert_eq!(h[2], hit(0.5, "cp/a.wav", 1.0));
    assert_eq!(h[3], hit(0.5, "sn/a.wav", 1.0));
}

#[test]
fn golden_star_repeat_subdivide() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"bd:1*2 sn:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(
        h,
        vec![
            hit(0.0, "bd/a.wav", 1.0),
            hit(0.25, "bd/a.wav", 1.0),
            hit(0.5, "sn/a.wav", 1.0),
        ]
    );
}

#[test]
fn golden_gain_lane_zip() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = r#"d1 $ s "bd:1 sn:1" # gain "1 0.5""#;
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(
        h,
        vec![hit(0.0, "bd/a.wav", 1.0), hit(0.5, "sn/a.wav", 0.5),]
    );
}

#[test]
fn golden_d3_prefix_orbit_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d3 $ s \"bd:2\"\n";
    let p = parse(src, 100, root).unwrap();
    assert_eq!(p.tracks[0].name, "d3");
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(h, vec![hit(0.0, "bd/b.wav", 1.0)]);
}

#[test]
fn golden_alias_in_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "alias kick bd:2\nd1 $ s \"kick sn:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(
        h,
        vec![hit(0.0, "bd/b.wav", 1.0), hit(0.5, "sn/a.wav", 1.0),]
    );
}

#[test]
fn golden_slow_two_alternates_across_cycles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ slow 2 $ s \"bd:1 sn:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let TrackKind::Pattern { tempo, .. } = &p.tracks[0].kind;
    assert_eq!(tempo, &Some(TempoWrap::Slow(2)));
    let h0 = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    let h1 = normalize_schedule(root, p.events_for_track(&p.tracks[0], 1, root).unwrap());
    assert_eq!(h0, vec![hit(0.0, "bd/a.wav", 1.0)]);
    assert_eq!(h1, vec![hit(0.0, "sn/a.wav", 1.0)]);
}

#[test]
fn golden_silence_skips_scheduling() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ s \"bd:1\"\nd2 $ s \"sn:1\"\nsilence d1\n";
    let p = parse(src, 100, root).unwrap();
    assert!(p.is_silenced("d1"));
    assert!(!p.is_silenced("d2"));
    let h0 = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    let h1 = normalize_schedule(root, p.events_for_track(&p.tracks[1], 0, root).unwrap());
    assert!(h0.is_empty());
    assert_eq!(h1, vec![hit(0.0, "sn/a.wav", 1.0)]);
}

#[test]
fn golden_track_pattern_gain_scales() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = "d1 $ gain 0.5 s \"bd:1\"\n";
    let p = parse(src, 100, root).unwrap();
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(h, vec![hit(0.0, "bd/a.wav", 0.5)]);
}

#[test]
fn golden_fixture_file_inline_seq() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fixture_standard_banks(root);
    let src = include_str!("fixtures/phase0_inline.seq");
    let p = parse(src, 100, root).unwrap();
    assert_eq!(p.tracks.len(), 1);
    let h = normalize_schedule(root, p.events_for_track(&p.tracks[0], 0, root).unwrap());
    assert_eq!(h, vec![hit(0.0, "sn/b.wav", 1.0)]);
}
