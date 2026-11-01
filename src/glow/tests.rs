use super::*;
use serde_json::json;

#[test]
fn colors_parse_in_the_usual_spellings() {
    assert_eq!(parse_color("#00D4FF"), Some(0x00D4FF));
    assert_eq!(parse_color("00d4ff"), Some(0x00D4FF));
    assert_eq!(parse_color(" #0df "), Some(0x00DDFF));
    assert_eq!(parse_color("#00D4F"), None);
    assert_eq!(parse_color("cyan"), None);
    assert_eq!(parse_color("#GG0000"), None);
    assert_eq!(format_color(0x00D4FF), "#00D4FF");
}

#[test]
fn the_default_is_on_and_soft_cyan() {
    let cfg = config_from(None);
    assert!(cfg.enabled);
    assert_eq!(cfg.color, 0x00D4FF);
    assert_eq!(cfg.thickness, 12);
    assert_eq!(cfg.linger_ms, 1_500);
    assert_eq!(config_from(Some(&json!({}))), GlowConfig::default());
}

#[test]
fn the_config_can_turn_it_off_or_recolor_it() {
    assert!(!config_from(Some(&json!({ "glow": false }))).enabled);
    assert!(!config_from(Some(&json!({ "glow": { "enabled": false } }))).enabled);
    let cfg = config_from(Some(&json!({ "glow": { "color": "#FF00FF", "thickness": 99, "linger_ms": 99_999 } })));
    assert!(cfg.enabled);
    assert_eq!(cfg.color, 0xFF00FF);
    assert_eq!(cfg.thickness, 24);
    assert_eq!(cfg.linger_ms, 10_000);
}

#[test]
fn a_broken_setting_falls_back_instead_of_turning_the_glow_off() {
    let cfg = config_from(Some(&json!({ "glow": { "enabled": "nope", "color": "blue", "thickness": -3 } })));
    assert_eq!(cfg, GlowConfig::default());
}

#[test]
fn a_bom_is_ignored() {
    assert_eq!(strip_bom(b"\xEF\xBB\xBF{}"), b"{}");
    assert_eq!(strip_bom(b"{}"), b"{}");
}

#[test]
fn only_desktop_actions_glow() {
    for c in ["click", "set-value", "type", "expand", "scroll", "launch", "press", "fill-secret", "batch", "mouse-click"] {
        assert!(glows_for(c), "{c}");
    }
    for c in ["snapshot", "find", "screenshot", "get", "is", "list-windows", "list-apps", "wait", "version", "session",
        "cursor-overlay", "trace", "clipboard-get", "clipboard-set", "skills", "jetpack note"] {
        assert!(!glows_for(c), "{c}");
    }
}

#[test]
fn the_flag_turns_it_off_and_reads_never_hold_it() {
    set_disabled_by_flag(true);
    assert!(begin("click").is_none());
    set_disabled_by_flag(false);
    assert!(begin("snapshot").is_none());
}

#[test]
fn helper_args_round_trip() {
    let a = HelperArgs { first_pid: Some(4321), config: GlowConfig { enabled: true, color: 0x12AB34, thickness: 9, linger_ms: 700, strict: true } };
    let argv = a.to_args();
    assert_eq!(argv[0], HELPER_ARG);
    assert_eq!(HelperArgs::parse(&argv[1..]), a);
    let junk: Vec<String> = ["--color", "nope", "--thickness", "x", "--pid"].iter().map(|s| s.to_string()).collect();
    let p = HelperArgs::parse(&junk);
    assert_eq!((p.first_pid, p.config.color, p.config.thickness), (None, DEFAULT_COLOR, DEFAULT_THICKNESS));
}

#[test]
fn the_frame_stays_up_while_held_and_lingers_after() {
    let mut h = Holders::new(1_500, 0);
    assert!(!h.visible(0));
    h.begin(10);
    assert!(h.visible(100));
    h.end(10, 1_000);
    assert!(h.visible(2_000));
    assert!(h.visible(2_499));
    assert!(!h.visible(2_500));
}

#[test]
fn a_new_command_inside_the_linger_keeps_it_up_without_a_gap() {
    let mut h = Holders::new(1_500, 0);
    h.begin(10);
    h.end(10, 1_000);
    h.begin(11);
    assert!(h.visible(5_000));
    h.end(11, 5_000);
    assert!(h.visible(6_400));
    assert!(!h.visible(6_500));
}

#[test]
fn two_holders_end_only_when_both_let_go() {
    let mut h = Holders::new(1_500, 0);
    h.begin(10);
    h.begin(11);
    h.end(10, 100);
    assert!(h.visible(10_000));
    h.end(11, 10_000);
    assert!(!h.visible(11_500));
}

#[test]
fn a_crashed_command_is_let_go_by_the_pid_check() {
    let mut h = Holders::new(1_500, 0);
    h.begin(10);
    h.prune(200, |_| false);
    assert!(h.pids().is_empty());
    assert!(h.visible(1_699));
    assert!(!h.visible(1_700));
}

#[test]
fn an_unmatched_end_is_harmless() {
    let mut h = Holders::new(1_500, 0);
    h.end(99, 0);
    assert!(h.visible(10));
    assert!(!h.visible(1_500));
}

#[test]
fn the_helper_leaves_after_a_minute_of_nothing() {
    let mut h = Holders::new(1_500, 0);
    h.begin(1);
    h.end(1, 1_000);
    assert!(h.visible(2_000));
    assert!(!h.visible(3_000));
    assert!(!h.should_exit(30_000));
    assert!(h.should_exit(62_000));
    h.begin(2);
    assert!(!h.should_exit(500_000));
}

#[test]
fn the_pulse_breathes_between_dim_and_full() {
    let samples: Vec<u8> = (0..PULSE_PERIOD_MS).step_by(10).map(pulse_alpha).collect();
    assert_eq!(*samples.iter().min().unwrap(), 155);
    assert_eq!(*samples.iter().max().unwrap(), 255);
    assert_eq!(pulse_alpha(0), pulse_alpha(PULSE_PERIOD_MS));
    assert_eq!(pulse_alpha(PULSE_PERIOD_MS / 2), 255);
    for w in samples.windows(2) {
        assert!((i16::from(w[0]) - i16::from(w[1])).abs() <= 3, "no jumps");
    }
}

#[test]
fn thickness_follows_scaling() {
    assert_eq!(scaled_thickness(6, 96), 6);
    assert_eq!(scaled_thickness(6, 144), 9);
    assert_eq!(scaled_thickness(6, 192), 12);
    assert_eq!(scaled_thickness(6, 0), 6);
    assert_eq!(scaled_thickness(1, 96), 2);
}

#[test]
fn four_strips_cover_the_edge_and_never_overlap() {
    let m = Rect { x: -1920, y: 0, w: 1920, h: 1080 };
    let s = strips(m, 6);
    assert_eq!(s.len(), 4);
    let area: i32 = s.iter().map(|r| r.w * r.h).sum();
    assert_eq!(area, 2 * 1920 * 6 + 2 * 6 * (1080 - 12));
    for (i, a) in s.iter().enumerate() {
        assert!(a.x >= m.x && a.y >= m.y && a.x + a.w <= m.x + m.w && a.y + a.h <= m.y + m.h);
        for b in &s[i + 1..] {
            let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
            assert!(apart, "{a:?} {b:?}");
        }
    }
    let tiny = strips(Rect { x: 0, y: 0, w: 4, h: 4 }, 6);
    assert_eq!(tiny.len(), 2);
}

#[test]
fn the_edge_is_solid_and_fades_inward() {
    assert_eq!(edge_alpha(-1, 6), 0);
    assert_eq!(edge_alpha(6, 6), 0);
    let ramp: Vec<u8> = (0..6).map(|d| edge_alpha(d, 6)).collect();
    assert!(ramp[0] > 200);
    assert!(ramp.windows(2).all(|w| w[0] > w[1]));
    assert!(ramp[5] > 0);
}

#[test]
fn pixels_are_premultiplied_in_the_chosen_color() {
    let m = Rect { x: 0, y: 0, w: 100, h: 50 };
    let top = strips(m, 6)[0];
    let px = render_strip(top, m, 6, 0x00D4FF);
    assert_eq!(px.len(), (top.w * top.h) as usize);
    let edge = px[50];
    let a = edge >> 24;
    assert!(a > 200);
    assert_eq!((edge >> 16) & 0xFF, 0);
    assert_eq!((edge >> 8) & 0xFF, (0xD4 * a + 127) / 255);
    assert_eq!(edge & 0xFF, (0xFF * a + 127) / 255);
    for p in &px {
        let a = p >> 24;
        assert!(((p >> 16) & 0xFF) <= a && ((p >> 8) & 0xFF) <= a && (p & 0xFF) <= a);
    }
    let corner = px[0];
    assert_eq!(corner >> 24, u32::from(edge_alpha(0, 6)));
    let left = strips(m, 6)[2];
    let lp = render_strip(left, m, 6, 0xFF0000);
    assert_eq!(lp[0] >> 24, u32::from(edge_alpha(0, 6)));
    assert_eq!(lp[5] >> 24, u32::from(edge_alpha(5, 6)));
}

#[test]
fn settings_survive_the_trip_in_a_message() {
    for cfg in [
        GlowConfig::default(),
        GlowConfig { enabled: true, color: 0xFF00FF, thickness: 24, linger_ms: 10_000, strict: true },
        GlowConfig { enabled: true, color: 0x000000, thickness: 2, linger_ms: 0, strict: false },
    ] {
        assert_eq!(unpack_config(pack_config(&cfg)), Some(cfg));
    }
    assert_eq!(unpack_config(0), None);
    let clamped = unpack_config(pack_config(&GlowConfig { enabled: true, color: 0x1FF_FFFF, thickness: 99, linger_ms: 99_999, strict: false }));
    assert_eq!(clamped, Some(GlowConfig { enabled: true, color: 0xFF_FFFF, thickness: 24, linger_ms: 10_000, strict: false }));
}

#[test]
fn a_new_linger_applies_to_the_next_end() {
    let mut h = Holders::new(1_500, 0);
    h.set_linger(6_000);
    h.begin(1);
    h.end(1, 1_000);
    assert!(h.visible(6_999));
    assert!(!h.visible(7_000));
}

#[test]
fn the_capture_exclusion_setting_reads_and_travels_with_every_begin() {
    assert!(!config_from(None).strict);
    assert!(config_from(Some(&json!({ "glow": { "require_capture_exclusion": true } }))).strict);
    let cfg = GlowConfig { strict: true, ..GlowConfig::default() };
    assert_eq!(unpack_config(pack_config(&cfg)), Some(cfg));
    let back = HelperArgs::parse(&HelperArgs { first_pid: None, config: cfg }.to_args()[1..]);
    assert!(back.config.strict);
}

#[test]
fn a_strip_windows_will_not_exclude_is_kept_unless_exclusion_is_required() {
    assert!(keep_unexcluded_strip(&GlowConfig::default()));
    assert!(!keep_unexcluded_strip(&GlowConfig { strict: true, ..GlowConfig::default() }));
}

#[test]
fn the_capture_mode_names_how_the_frame_stays_out_of_screenshots() {
    assert_eq!(capture_mode(0, 0), None);
    assert_eq!(capture_mode(4, 4), Some(CaptureMode::Excluded));
    assert_eq!(capture_mode(3, 4), Some(CaptureMode::HideForCapture));
    assert_eq!(capture_mode(0, 4), Some(CaptureMode::HideForCapture));
}

#[test]
fn a_frame_that_failed_to_build_is_retried_slowly_not_every_tick() {
    assert!(rebuild_due(true, None, 0));
    assert!(!rebuild_due(false, None, 0));
    assert!(!rebuild_due(true, Some(1_000), 1_040));
    assert!(rebuild_due(true, Some(1_000), 1_000 + REBUILD_RETRY_MS));
}

#[test]
fn a_capture_hide_ends_when_shown_timed_out_or_its_process_died() {
    let mut h = CaptureHides::default();
    assert!(!h.active(0, |_| true));
    h.hide(7, 100);
    assert!(h.active(200, |_| true));
    h.show(7);
    assert!(!h.active(200, |_| true));
    h.hide(8, 100);
    assert!(!h.active(100 + HIDE_MAX_MS, |_| true), "a hide never outlives its deadline");
    h.hide(9, 0);
    assert!(!h.active(10, |p| p != 9), "a dead capturer can't keep the frame hidden");
}

#[test]
fn off_windows_a_capture_hide_does_nothing() {
    let hide = hide_for_capture();
    #[cfg(not(target_os = "windows"))]
    {
        assert!(hide.mask.is_empty());
        assert!(hide.strips_to_mask().is_empty());
    }
    drop(hide);
}
