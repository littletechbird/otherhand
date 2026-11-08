//! The glow's work session.
//!
//! An acting command opens it. It stays open across later commands, including
//! headless ones that never move the foreground, until every client has gone
//! and `SESSION_IDLE_MS` passes with no new command,
//! or until [`WorkSession::release`]. A person switching windows does not
//! close it. `linger_ms` is not part of this clock.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct WorkSession {
    started: bool,
    released: bool,
    open_until: Option<u64>,
}

impl WorkSession {
    pub(crate) fn begin(&mut self) {
        self.started = true;
        self.released = false;
        self.open_until = None;
    }

    pub(crate) fn is_open(&self, now: u64) -> bool {
        !self.released && self.open_until.is_some_and(|deadline| now < deadline)
    }

    /// The last client has gone. Arms the clear-idle clock once.
    pub(crate) fn clients_idle(&mut self, now: u64, idle_ms: u64) {
        if !self.started || self.released || self.open_until.is_some() {
            return;
        }
        self.open_until = Some(now.saturating_add(idle_ms));
    }

    /// The idle clock ran out. The returned instant is when the session released.
    pub(crate) fn take_expired(&mut self, now: u64) -> Option<u64> {
        let deadline = self.open_until?;
        if self.released || now < deadline {
            return None;
        }
        self.released = true;
        self.open_until = None;
        Some(deadline)
    }

    pub(crate) fn release(&mut self) {
        self.released = true;
        self.open_until = None;
    }

    pub(crate) fn is_released(&self) -> bool {
        self.released
    }
}
