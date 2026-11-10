    use super::*;

    fn obs(fg: isize, x: i32, tick: u32) -> Observation {
        Observation { foreground: fg, cursor: (x, 5), last_input_tick: tick }
    }

    #[test]
    fn unchanged_handle_and_cursor_is_clean_even_if_titles_would_differ() {
        let c = compare(&obs(42, 1, 7), &obs(42, 1, 7));
        assert!(!c.stolen());
    }

    #[test]
    fn a_new_foreground_handle_is_stolen() {
        let c = compare(&obs(42, 1, 7), &obs(43, 1, 7));
        assert!(c.stolen() && c.foreground_changed && !c.cursor_moved);
    }

    #[test]
    fn a_moved_cursor_is_stolen_and_user_input_is_reported() {
        let c = compare(&obs(42, 1, 7), &obs(42, 2, 9));
        assert!(c.stolen() && c.cursor_moved && c.user_input_seen);
    }

    #[test]
    fn the_person_moving_the_mouse_is_not_theft() {
        let c = compare(&obs(42, 1, 7), &obs(42, 9, 8));
        assert!(user_only(c));
        let c = compare(&obs(42, 1, 7), &obs(43, 9, 8));
        assert!(!user_only(c));
    }

    #[test]
    fn the_error_carries_the_guard_code() {
        let e = stolen_error("click", Change { foreground_changed: true, ..Default::default() }, false);
        let AppError::Adapter(a) = e else { panic!("adapter error") };
        assert_eq!(a.code, ErrorCode::FocusStolen);
    }

    #[test]
    fn headed_commands_skip_the_guard() {
        let r = guard("click", true, || Ok(json!({"ok": 1}))).unwrap();
        assert_eq!(r["ok"], 1);
    }

    fn t(at_ms: u64, fg: isize) -> Timed {
        Timed { at_us: at_ms * 1000, obs: obs(fg, 1, 7) }
    }

    #[test]
    fn away_measures_each_stretch_and_whether_it_came_back() {
        let a = away(&obs(42, 1, 7), &[t(10, 99), t(14, 42), t(50, 77), t(53, 42)], &obs(42, 1, 7), 200_000);
        assert_eq!((a.stretches, a.longest_ms, a.total_ms, a.back), (2, 4, 7, true));
    }

    #[test]
    fn an_open_stretch_runs_to_the_end_and_is_not_back() {
        let a = away(&obs(42, 1, 7), &[t(10, 99)], &obs(99, 1, 7), 400_000);
        assert!(!a.back);
        assert_eq!(a.longest_ms, 390);
    }

    #[test]
    fn a_quick_put_back_is_restored_not_stolen() {
        let change = Change { foreground_changed: true, ..Default::default() };
        let a = Away { stretches: 1, longest_ms: 3, total_ms: 3, back: true };
        assert_eq!(decide(change, a, ShieldReport::default()), Verdict::Restored);
    }

    #[test]
    fn a_slow_put_back_is_still_stolen() {
        let change = Change { foreground_changed: true, ..Default::default() };
        let a = Away { stretches: 1, longest_ms: RESTORE_BUDGET_MS + 1, total_ms: RESTORE_BUDGET_MS + 1, back: true };
        assert_eq!(decide(change, a, ShieldReport::default()), Verdict::Stolen);
    }

    #[test]
    fn a_foreground_that_never_came_back_is_stolen() {
        let change = Change { foreground_changed: true, ..Default::default() };
        let a = Away { stretches: 1, longest_ms: 2, total_ms: 2, back: false };
        assert_eq!(decide(change, a, ShieldReport::default()), Verdict::Stolen);
    }

    #[test]
    fn a_moved_cursor_is_never_restored_away() {
        let change = Change { foreground_changed: true, cursor_moved: true, ..Default::default() };
        let a = Away { stretches: 1, longest_ms: 1, total_ms: 1, back: true };
        assert_eq!(decide(change, a, ShieldReport::default()), Verdict::Stolen);
    }

    #[test]
    fn the_persons_own_switch_is_reported_not_fought() {
        let change = Change { foreground_changed: true, ..Default::default() };
        let a = Away { stretches: 1, longest_ms: 1, total_ms: 1, back: true };
        let shield = ShieldReport { person_switch: true, ..Default::default() };
        assert_eq!(decide(change, a, shield), Verdict::Stolen);
    }

    #[test]
    fn nothing_changed_is_clean() {
        assert_eq!(decide(Change::default(), Away { back: true, ..Default::default() }, ShieldReport::default()), Verdict::Clean);
    }

    #[test]
    fn person_switch_window_handles_tick_wraparound() {
        assert!(is_person_switch(100, Some(u32::MAX - 50)));
        assert!(!is_person_switch(10_000, Some(1_000)));
        assert!(!is_person_switch(10_000, None));
        assert!(is_person_switch(10_000, Some(10_000 - PERSON_SWITCH_WINDOW_MS)));
    }

    #[test]
    fn restored_results_carry_the_note_and_errors_pass_through() {
        let a = Away { stretches: 1, longest_ms: 4, total_ms: 4, back: true };
        let v = annotate(Ok(json!({"action": "click"})), restored_note(a, ShieldReport { restore_attempts: 1, ..Default::default() })).unwrap();
        assert_eq!(v["focus_guard"]["status"], "RESTORED");
        assert_eq!(v["focus_guard"]["away_ms"], 4);
        assert_eq!(v["action"], "click");
        let e = annotate(Err(stolen_error("click", Change::default(), false)), json!({}));
        assert!(e.is_err());
    }

    #[test]
    fn the_stolen_error_says_whether_the_foreground_came_back() {
        let change = Change { foreground_changed: true, ..Default::default() };
        let e = stolen_error_with("select", change, false, Away { stretches: 1, longest_ms: 900, total_ms: 900, back: true }, ShieldReport { restore_attempts: 3, ..Default::default() });
        let AppError::Adapter(a) = e else { panic!("adapter error") };
        let d = a.details.clone().expect("details");
        assert_eq!(d["status"], "STOLEN");
        assert_eq!(d["foreground_restored"], true);
        assert_eq!(d["away_ms"], 900);
        assert_eq!(d["restore_attempts"], 3);
    }

    #[test]
    fn a_restored_error_keeps_its_code_and_carries_the_note() {
        let refused = AdapterError::new(ErrorCode::PolicyDenied, "menu").with_details(json!({ "kind": "win32_menu_headless" }));
        let out = annotate(Err(refused.into()), json!({ "status": "RESTORED" })).unwrap_err();
        let AppError::Adapter(a) = out else { panic!("adapter error") };
        assert_eq!(a.code, ErrorCode::PolicyDenied);
        let d = a.details.unwrap();
        assert_eq!(d["kind"], "win32_menu_headless");
        assert_eq!(d["focus_guard"]["status"], "RESTORED");
    }

    #[test]
    fn an_error_without_details_gets_them() {
        let out = annotate(Err(AdapterError::new(ErrorCode::ActionFailed, "x").into()), json!({ "status": "RESTORED" })).unwrap_err();
        let AppError::Adapter(a) = out else { panic!("adapter error") };
        assert_eq!(a.details.unwrap()["focus_guard"]["status"], "RESTORED");
    }

    #[test]
    fn a_non_adapter_error_is_tagged_too() {
        let out = annotate(Err(AppError::Internal("boom".into())), json!({ "status": "RESTORED" })).unwrap_err();
        let AppError::Adapter(a) = out else { panic!("adapter error") };
        assert_eq!(a.code, ErrorCode::Internal);
        assert_eq!(a.message, "boom");
        assert_eq!(a.details.unwrap()["focus_guard"]["status"], "RESTORED");
    }

    #[test]
    fn every_foreground_change_gets_a_verdict_that_is_tagged() {
        let change = Change { foreground_changed: true, user_input_seen: true, ..Default::default() };
        for (away_ms, back) in [(17, true), (400, true), (5, false)] {
            let a = Away { stretches: 1, longest_ms: away_ms, total_ms: away_ms, back };
            let v = decide(change, a, ShieldReport::default());
            assert!(matches!(v, Verdict::Restored | Verdict::Stolen), "{away_ms} {back}: {v:?}");
        }
    }

    #[test]
    fn a_blind_foreground_blocks_a_focus_command_and_not_a_read() {
        assert!(blocks_when_blind("type", ForegroundRead::Blind, true));
        assert!(blocks_when_blind("press", ForegroundRead::Seen, false));
        assert!(!blocks_when_blind("press", ForegroundRead::Seen, true));
        assert!(!blocks_when_blind("snapshot", ForegroundRead::Blind, false));
        assert!(!blocks_when_blind("type", ForegroundRead::Unsupported, false));
    }
