use super::*;
use clap::CommandFactory;

#[test]
fn a_focus_command_waits_out_a_non_zero_countdown_after_a_visible_ack() {
    let ack = FrameAck {
        strips: 4,
        min_alpha: 200,
    };
    let decision = decide(ShowOutcome::Shown(ack), 0);
    assert!(decision.block.is_none());
    assert!(decision.report.shown);
    assert_eq!(decision.report.mode, "warning");
    assert!(decision.wait_ms >= MIN_COUNTDOWN_MS);
    assert!(!input_allowed(Some(ack), 0, decision.wait_ms));
    assert!(!input_allowed(
        Some(ack),
        decision.wait_ms - 1,
        decision.wait_ms
    ));
    assert!(input_allowed(Some(ack), decision.wait_ms, decision.wait_ms));
}

#[test]
fn a_dark_or_missing_frame_refuses_before_any_wait() {
    let missing = decide(
        ShowOutcome::Unavailable {
            error: "helper did not answer".into(),
        },
        DEFAULT_COUNTDOWN_MS,
    );
    assert!(matches!(missing.block, Some(Block::NotShown { .. })));
    assert_eq!(missing.wait_ms, 0);
    assert!(!missing.report.shown);
    let dark = decide(
        ShowOutcome::Shown(FrameAck {
            strips: 4,
            min_alpha: 0,
        }),
        DEFAULT_COUNTDOWN_MS,
    );
    assert!(dark.block.is_some());
    assert!(!input_allowed(
        Some(FrameAck {
            strips: 0,
            min_alpha: 200
        }),
        5_000,
        1_500
    ));
    assert!(!input_allowed(None, 5_000, 1_500));
}

#[test]
fn the_person_off_switch_refuses_and_a_longer_countdown_is_honored() {
    let off = decide(ShowOutcome::PersonOff, DEFAULT_COUNTDOWN_MS);
    assert!(matches!(off.block, Some(Block::PersonOff)));
    assert_eq!(off.report.mode, "off");
    let longer = decide(
        ShowOutcome::Shown(FrameAck {
            strips: 1,
            min_alpha: 10,
        }),
        4_000,
    );
    assert_eq!(longer.wait_ms, 4_000);
    assert_eq!(clamp_countdown(50), MIN_COUNTDOWN_MS);
    assert_eq!(clamp_countdown(99_000), MAX_COUNTDOWN_MS);
}

#[test]
fn the_countdown_steps_are_visible_and_the_ack_round_trips() {
    let pulse = 200;
    assert_eq!(countdown_brightness(pulse, 0, 1_500), 80);
    assert_eq!(countdown_brightness(pulse, 600, 1_500), 140);
    assert_eq!(countdown_brightness(pulse, 1_200, 1_500), 200);
    assert_eq!(countdown_brightness(pulse, 1_500, 1_500), pulse);
    assert!(countdown_brightness(pulse, 0, 1_500) < countdown_brightness(pulse, 1_200, 1_500));
    let packed = pack_ack(4, 180);
    assert_eq!(
        parse_ack(packed),
        Some(FrameAck {
            strips: 4,
            min_alpha: 180
        })
    );
    assert_eq!(parse_ack(0), None);
    assert_eq!(pack_ack(0, 180), 0);
}

#[test]
fn every_command_name_is_in_exactly_one_warning_list() {
    let names: Vec<String> = crate::cli::Cli::command()
        .get_subcommands()
        .map(|cmd| cmd.get_name().to_string())
        .collect();
    assert!(!names.is_empty());
    for name in &names {
        assert!(
            known_command(name),
            "{name} has no focus-warning classification"
        );
        assert!(
            !(warning_required(name) && exempt_mutation(name)),
            "{name} cannot both warn and be exempt"
        );
    }
    let classified = names.iter().filter(|name| known_command(name)).count();
    assert_eq!(classified, names.len());
}

#[test]
fn a_helper_mutex_is_named_per_user_and_images_compare_literally() {
    assert_eq!(
        mutex_name(Some("S-1-5-21-1")),
        "Local\\OtherhandGlowHelper-S-1-5-21-1"
    );
    assert_eq!(mutex_name(Some("..\\evil")), "Local\\OtherhandGlowHelper");
    let left = std::path::Path::new(r"\\?\C:\Apps\Otherhand.exe");
    let right = std::path::Path::new(r"C:/Apps/otherhand.exe");
    assert!(same_image(left, right));
    assert!(!same_image(
        left,
        std::path::Path::new(r"C:\Apps\other.exe")
    ));
}

fn clipboard_and_the_cursor_overlay_do_not_warn_and_type_does() {
    assert!(warning_required("type"));
    assert!(warning_required("press"));
    assert!(warning_required("launch"));
    assert!(warning_required("focus-window"));
    assert!(!warning_required("clipboard-set"));
    assert!(!warning_required("clipboard-clear"));
    assert!(!warning_required("cursor-overlay"));
    assert!(exempt_mutation("clipboard-set"));
    assert!(!warning_required("snapshot"));
}
