use super::*;

#[test]
fn toggling_a_two_state_box_flips_it() {
    assert_eq!(checkbox_target(BS_AUTOCHECKBOX, 0, CheckAction::Toggle), Some(1));
    assert_eq!(checkbox_target(BS_AUTOCHECKBOX, 1, CheckAction::Toggle), Some(0));
    assert_eq!(checkbox_target(BS_CHECKBOX, 1, CheckAction::Toggle), Some(0));
}

#[test]
fn toggling_a_three_state_box_cycles() {
    assert_eq!(checkbox_target(BS_AUTO3STATE, 0, CheckAction::Toggle), Some(1));
    assert_eq!(checkbox_target(BS_AUTO3STATE, 1, CheckAction::Toggle), Some(2));
    assert_eq!(checkbox_target(BS_3STATE, 2, CheckAction::Toggle), Some(0));
}

#[test]
fn check_and_uncheck_name_their_target() {
    assert_eq!(checkbox_target(BS_AUTOCHECKBOX, 0, CheckAction::Check), Some(1));
    assert_eq!(checkbox_target(BS_AUTOCHECKBOX, 1, CheckAction::Uncheck), Some(0));
}

/// Auto radio buttons must also clear their siblings, which only a real
/// click does, so they are not handled here.
#[test]
fn radio_buttons_and_other_buttons_are_left_to_uia() {
    for kind in [0x0, 0x1, 0x4, 0x7, 0x9, 0xB] {
        assert_eq!(checkbox_target(kind, 0, CheckAction::Toggle), None, "kind {kind:#x}");
    }
}

#[test]
fn push_kinds_are_the_plain_click_ones() {
    for kind in [0x0, 0x1, 0xC, 0xD, 0xE, 0xF] {
        assert!(is_push_kind(kind), "kind {kind:#x}");
    }
    for kind in [0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x9] {
        assert!(!is_push_kind(kind), "kind {kind:#x}");
    }
}

#[test]
fn bn_clicked_carries_the_control_id_in_the_low_word() {
    assert_eq!(bn_clicked_wparam(0x1234), 0x1234);
    assert_eq!(bn_clicked_wparam(-1), 0xFFFF);
}

fn item(framework: &str) -> MenuFacts {
    MenuFacts { is_menu_item: true, framework: framework.into(), ..MenuFacts::default() }
}

/// Sandbox run 3: Notepad's File item got past a `FrameworkId == "Win32"`
/// test. The MSAA proxy can report an empty framework; that must refuse too.
#[test]
fn an_msaa_menu_bar_item_with_no_framework_is_classic() {
    assert!(classic_menu_item(&item("")));
    assert!(classic_menu_item(&item("MSAA")));
    assert!(classic_menu_item(&item(" win32 ")));
}

#[test]
fn self_drawn_menus_are_left_alone_unless_user32_owns_the_bar() {
    for f in ["Chrome", "XAML", "DirectUI", "WPF", "WinForm"] {
        assert!(!classic_menu_item(&item(f)), "{f}");
        let owned = MenuFacts { owner_has_win32_menu: true, parent_is_menu_bar: true, ..item(f) };
        assert!(classic_menu_item(&owned), "{f} on a USER32 menu bar");
    }
}

#[test]
fn an_unknown_framework_counts_only_with_a_real_hmenu() {
    assert!(!classic_menu_item(&item("SomethingNew")));
    assert!(classic_menu_item(&MenuFacts { owner_has_win32_menu: true, ..item("SomethingNew") }));
}

#[test]
fn only_menu_items_are_menu_items() {
    let not_item = MenuFacts { is_menu_item: false, owner_has_win32_menu: true, parent_is_menu_bar: true, ..item("Win32") };
    assert!(!classic_menu_item(&not_item));
    assert!(!refuse_menu_action(&not_item, true, true));
}

#[test]
fn headless_expand_on_a_classic_item_is_always_refused() {
    let file = MenuFacts { parent_is_menu_bar: true, owner_has_win32_menu: true, ..item("") };
    assert!(refuse_menu_action(&file, true, false));
    assert!(refuse_menu_action(&file, true, true));
}

#[test]
fn clicks_are_refused_on_the_bar_or_when_they_open_a_submenu() {
    let on_bar = MenuFacts { parent_is_menu_bar: true, ..item("Win32") };
    assert!(refuse_menu_action(&on_bar, false, false));
    let in_popup = item("Win32");
    assert!(refuse_menu_action(&in_popup, false, true));
    assert!(!refuse_menu_action(&in_popup, false, false));
}
