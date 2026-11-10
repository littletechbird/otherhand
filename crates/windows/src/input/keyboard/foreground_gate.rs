//! Stops a keystroke sequence when the foreground window changes, or when
//! it cannot be read at all.

use agent_desktop_core::{AdapterError, DeliverySemantics, ErrorCode};

#[derive(Debug)]
pub(crate) struct ForegroundGate {
    baseline: isize,
}

impl ForegroundGate {
    pub(crate) fn arm() -> Result<Self, AdapterError> {
        Ok(Self { baseline: read()? })
    }

    pub(crate) fn ensure(&self, sent: bool) -> Result<(), AdapterError> {
        let now = read()?;
        if changed_from(self.baseline, Some(now)) {
            Err(changed(sent))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn changed_from(before: isize, now: Option<isize>) -> bool {
    match now {
        Some(hwnd) if hwnd != 0 && hwnd != before => true,
        None => true,
        Some(_) => false,
    }
}

fn changed(sent: bool) -> AdapterError {
    let disposition = if sent {
        DeliverySemantics::delivered_unverified()
    } else {
        DeliverySemantics::not_delivered()
    };
    AdapterError::new(
        ErrorCode::FocusStolen,
        "Foreground changed while input was being sent; the remaining input was not sent",
    )
    .with_disposition(disposition)
}

fn blind() -> AdapterError {
    AdapterError::new(
        ErrorCode::FocusStolen,
        "The foreground could not be read, so input was not sent",
    )
    .with_disposition(DeliverySemantics::not_delivered())
}

#[cfg(not(target_os = "windows"))]
fn read() -> Result<isize, AdapterError> {
    Ok(1)
}

#[cfg(target_os = "windows")]
fn read() -> Result<isize, AdapterError> {
    let hwnd = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    if hwnd.is_null() {
        Err(blind())
    } else {
        Ok(hwnd as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_foreground_aborts_and_an_unreadable_one_does_too() {
        assert!(changed_from(10, Some(11)));
        assert!(!changed_from(10, Some(10)));
        assert!(changed_from(10, None));
        assert!(!changed_from(10, Some(0)));
    }
}
