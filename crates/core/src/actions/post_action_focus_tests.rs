use super::*;

fn type_after_focus(headed: bool, already_focused: bool) -> Result<ActionResult, AdapterError> {
    let states = if already_focused {
        vec!["focused"]
    } else {
        vec![]
    };
    let mut adapter = adapter(
        element(Some("before"), &states),
        element(Some("inserted"), &["focused"]),
    );
    adapter.selection = Some(0..0);
    let action = Action::TypeText("inserted".into());
    let request = if headed {
        ActionRequest::headed(action)
    } else {
        ActionRequest::headless(action)
    };
    execute_verified_action(
        &adapter,
        &NativeHandle::null(),
        request,
        &InteractionLease::guarded(Deadline::after(1000).unwrap(), ()).unwrap(),
    )
}

#[test]
fn headed_typing_without_a_trusted_selection_verifies_by_the_new_text_alone() {
    let result = type_after_focus(true, false).unwrap();
    assert_eq!(
        result.disposition(),
        DeliverySemantics::delivered_verified()
    );
    assert_eq!(
        result.post_state.unwrap().value.as_deref(),
        Some("inserted")
    );
}

#[test]
fn focused_and_headless_typing_still_reject_contradictory_insertions() {
    for (headed, focused) in [(true, true), (false, false)] {
        let error = type_after_focus(headed, focused).unwrap_err();
        assert_eq!(error.code, ErrorCode::ActionFailed);
        assert_eq!(error.disposition, DeliverySemantics::delivered_unverified());
    }
}
