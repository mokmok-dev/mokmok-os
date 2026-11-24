/// Where a session sits in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionPhase {
    /// Created, no worker has taken it.
    Pending,
    /// A worker owns it but has not started work.
    Claimed,
    /// Work is under way.
    Running,
    /// The claim lapsed, so the session is available to be reclaimed.
    Expired,
    /// Work finished successfully. Terminal.
    Succeed,
    /// Work ended in failure. Terminal.
    Failed,
}

impl SessionPhase {
    /// Every phase.
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Claimed,
        Self::Running,
        Self::Expired,
        Self::Succeed,
        Self::Failed,
    ];

    /// Whether no event can leave this phase.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeed | Self::Failed)
    }
}

/// Something that can happen to a session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionEvent {
    /// Take a pending session.
    Claim,
    /// Begin work on a claimed session.
    Start,
    /// The claim lapsed.
    Expire,
    /// Return an expired session to pending.
    Retry,
    /// Work finished successfully.
    Succeed,
    /// Work ended in failure.
    Fail,
}

impl SessionEvent {
    /// Every event.
    pub const ALL: [Self; 6] = [
        Self::Claim,
        Self::Start,
        Self::Expire,
        Self::Retry,
        Self::Succeed,
        Self::Fail,
    ];
}

/// The single source of truth for which event may follow which phase.
#[must_use]
pub const fn transition(phase: SessionPhase, event: SessionEvent) -> Option<SessionPhase> {
    use SessionEvent as E;
    use SessionPhase as P;
    match (phase, event) {
        (P::Pending, E::Claim) => Some(P::Claimed),
        (P::Claimed, E::Start) => Some(P::Running),
        (P::Claimed | P::Running, E::Expire) => Some(P::Expired),
        (P::Expired, E::Retry) => Some(P::Pending),
        (P::Running, E::Succeed) => Some(P::Succeed),
        (P::Running, E::Fail) => Some(P::Failed),
        _ => None,
    }
}

/// A session as a caller stores it and as the model checker sees it.
///
/// A session starts at [`Session::pending`] and only ever changes through [`Session::apply`],
/// so no caller can build a finished session or revive one that has already finished.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Session {
    phase: SessionPhase,
    ever_terminal: bool,
    was_expired: bool,
}

impl Session {
    /// A fresh session, before anything has happened to it.
    #[must_use]
    pub const fn pending() -> Self {
        Self {
            phase: SessionPhase::Pending,
            ever_terminal: false,
            was_expired: false,
        }
    }

    /// Advances the session along [`transition`], returning `None` when no edge exists for
    /// `event` in the current phase.
    #[must_use]
    pub fn apply(self, event: SessionEvent) -> Option<Self> {
        let next = transition(self.phase, event)?;
        Some(Self {
            phase: next,
            ever_terminal: self.ever_terminal || next.is_terminal(),
            was_expired: self.was_expired || next == SessionPhase::Expired,
        })
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(self) -> SessionPhase {
        self.phase
    }

    /// Whether the session has ever been in a terminal phase.
    #[must_use]
    pub const fn ever_terminal(self) -> bool {
        self.ever_terminal
    }

    /// Whether the session has ever been in [`SessionPhase::Expired`].
    #[must_use]
    pub const fn was_expired(self) -> bool {
        self.was_expired
    }
}
