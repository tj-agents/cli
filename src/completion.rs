//! Durable, host-neutral completion state for one launch and native session.
//!
//! Hosts and the wrapper supply every observed fact.  This module deliberately
//! does not inspect transcripts, infer idleness, or perform external actions.

mod storage;

pub use storage::{Store, WireError};

const SCHEMA: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("{kind} must be nonempty ASCII letters, digits, '-' or '_'")]
    Unsafe { kind: &'static str },
    #[error("session handle must be host:id, with safe nonempty host and id")]
    Handle,
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },
}

fn safe_path_id(value: &str, kind: &'static str) -> Result<(), IdError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(IdError::Unsafe { kind });
    }
    Ok(())
}

fn opaque(value: &str, kind: &'static str) -> Result<(), IdError> {
    if value.trim().is_empty() {
        return Err(IdError::Empty { kind });
    }
    Ok(())
}

/// The exact launch and native-session pair whose completion is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identity {
    launch_id: String,
    session_handle: String,
}

impl Identity {
    pub fn new(launch_id: impl Into<String>, session_handle: impl Into<String>) -> Result<Self, IdError> {
        let launch_id = launch_id.into();
        let session_handle = session_handle.into();
        safe_path_id(&launch_id, "launch id")?;
        let Some((host, session)) = session_handle.split_once(':') else { return Err(IdError::Handle) };
        if session.contains(':')
            || safe_path_id(host, "session host").is_err()
            || safe_path_id(session, "session id").is_err()
        {
            return Err(IdError::Handle);
        }
        Ok(Self { launch_id, session_handle })
    }

    pub fn launch_id(&self) -> &str {
        &self.launch_id
    }
    pub fn session_handle(&self) -> &str {
        &self.session_handle
    }
}

/// An opaque native turn identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Turn(String);

impl Turn {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        opaque(&value, "turn token")?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An identity for one externally retried lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventId(String);

impl EventId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        safe_path_id(&value, "event id")?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A caller-supplied receipt for an external action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    id: String,
}

impl Receipt {
    pub fn new(id: impl Into<String>) -> Result<Self, IdError> {
        let id = id.into();
        opaque(&id, "receipt id")?;
        Ok(Self { id })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The assistant's final report observed at a native turn boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report(String);

impl Report {
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        opaque(&value, "final report")?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a session is explicitly waiting rather than requesting completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    User,
    Child,
    Ci,
    Approval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitReason {
    kind: WaitKind,
    description: String,
}

impl WaitReason {
    pub fn new(kind: WaitKind, description: impl Into<String>) -> Result<Self, IdError> {
        let description = description.into();
        opaque(&description, "wait description")?;
        Ok(Self { kind, description })
    }
    pub fn kind(&self) -> WaitKind {
        self.kind
    }
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// One external closeout step whose receipt is stored in the durable outbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Delivery,
    Tracker,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    step: Step,
    description: String,
}

impl Failure {
    pub fn new(step: Step, description: impl Into<String>) -> Result<Self, IdError> {
        let description = description.into();
        opaque(&description, "failure description")?;
        Ok(Self { step, description })
    }
    pub fn step(&self) -> Step {
        self.step
    }
    pub fn description(&self) -> &str {
        &self.description
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    #[error("event {0:?} conflicts with an earlier event carrying different facts")]
    Conflict(EventId),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NewInputError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("a completed record cannot accept another input")]
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FinishError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("finish requires an active turn")]
    NotActive,
    #[error("finish turn does not match the active turn")]
    TurnMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("waiting requires an active turn")]
    NotActive,
    #[error("wait turn does not match the active turn")]
    TurnMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndTurnError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("turn end does not match the current turn")]
    TurnMismatch,
    #[error("a requested finish requires a nonempty final report")]
    MissingReport,
    #[error("turn end is not valid after result delivery or completion")]
    NotCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InterruptError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("interruption does not match the current turn")]
    TurnMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HoldError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("a completed session cannot change its keep-open hold")]
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloseError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("close requires a ready result")]
    NotReady,
    #[error("keep-open hold prevents close")]
    Held,
    #[error("expected revision {expected} does not match current revision {actual}")]
    Revision { expected: u64, actual: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("close revision {expected} is no longer current")]
    ClosingRevision { expected: u64 },
    #[error("{0:?} receipt is already recorded")]
    AlreadyRecorded(Step),
    #[error("an external receipt can be recorded only while closing")]
    NotClosing,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompleteError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("close revision {expected} is no longer current")]
    ClosingRevision { expected: u64 },
    #[error("completion requires all delivery, tracker, and cleanup receipts")]
    PendingSteps,
    #[error("completion is valid only while closing")]
    NotClosing,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FailError {
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("close revision {expected} is no longer current")]
    ClosingRevision { expected: u64 },
    #[error("{0:?} receipt is already recorded")]
    AlreadyRecorded(Step),
    #[error("a failed step requires its current result or closeout revision")]
    NotClosing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResultReceipt {
    event: EventId,
}

/// Durable outcomes and closeout progress.  Its fields are available through getters only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    turn: Turn,
    report: Report,
    receipt: ResultReceipt,
    outbox: Outbox,
}

impl Outcome {
    pub fn turn(&self) -> &Turn {
        &self.turn
    }
    pub fn report(&self) -> &Report {
        &self.report
    }
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StepState {
    Pending,
    Recorded(Receipt),
    Failed(Failure),
}

/// Explicit receipt progression for delivery storage, tracker completion, and cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbox {
    delivery: StepState,
    tracker: StepState,
    cleanup: StepState,
}

impl Outbox {
    fn new() -> Self {
        Self { delivery: StepState::Pending, tracker: StepState::Pending, cleanup: StepState::Pending }
    }
    pub fn receipt(&self, step: Step) -> Option<&Receipt> {
        match self.step(step) {
            StepState::Recorded(receipt) => Some(receipt),
            StepState::Pending | StepState::Failed(_) => None,
        }
    }
    pub fn failure(&self, step: Step) -> Option<&Failure> {
        match self.step(step) {
            StepState::Failed(failure) => Some(failure),
            StepState::Pending | StepState::Recorded(_) => None,
        }
    }
    pub fn complete(&self) -> bool {
        [Step::Delivery, Step::Tracker, Step::Cleanup].into_iter().all(|step| self.receipt(step).is_some())
    }
    fn step(&self, step: Step) -> &StepState {
        match step {
            Step::Delivery => &self.delivery,
            Step::Tracker => &self.tracker,
            Step::Cleanup => &self.cleanup,
        }
    }
    fn step_mut(&mut self, step: Step) -> &mut StepState {
        match step {
            Step::Delivery => &mut self.delivery,
            Step::Tracker => &mut self.tracker,
            Step::Cleanup => &mut self.cleanup,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Withdrawal {
    turn: Turn,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Archived {
    outcome: Outcome,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Applied {
    id: EventId,
    operation: &'static str,
    facts: String,
}

/// Stored lifecycle state. Variant payloads stay private so only transition methods establish invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Inactive,
    Active { turn: Turn, intent: Option<Intent> },
    Waiting { turn: Turn, reason: WaitReason, report: Option<Report>, finish_requested: bool },
    Attention { reason: String, report: Option<Report>, outcome: Option<Outcome> },
    Ready { outcome: Outcome },
    Closing { revision: u64, outcome: Outcome },
    Completed { outcome: Outcome },
}

impl State {
    pub fn turn(&self) -> Option<&Turn> {
        match self {
            Self::Active { turn, .. } | Self::Waiting { turn, .. } => Some(turn),
            _ => None,
        }
    }
    pub fn outcome(&self) -> Option<&Outcome> {
        match self {
            Self::Attention { outcome, .. } => outcome.as_ref(),
            Self::Ready { outcome } | Self::Closing { outcome, .. } | Self::Completed { outcome } => Some(outcome),
            _ => None,
        }
    }
    pub fn closing_revision(&self) -> Option<u64> {
        match self {
            Self::Closing { revision, .. } => Some(*revision),
            _ => None,
        }
    }
}

/// A versioned completion record for exactly one launch and native session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    identity: Identity,
    revision: u64,
    hold: bool,
    state: State,
    withdrawals: Vec<Withdrawal>,
    archive: Vec<Archived>,
    applied: Vec<Applied>,
}

impl Record {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            revision: 0,
            hold: false,
            state: State::Inactive,
            withdrawals: Vec::new(),
            archive: Vec::new(),
            applied: Vec::new(),
        }
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn held_open(&self) -> bool {
        self.hold
    }
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn new_input(&mut self, event: EventId, turn: Turn) -> Result<(), NewInputError> {
        let facts = facts(&[turn.as_str()]);
        if self.replayed(&event, "new_input", &facts).map_err(NewInputError::Event)? {
            return Ok(());
        }
        let prior = std::mem::replace(&mut self.state, State::Inactive);
        match prior {
            State::Completed { outcome } => {
                self.state = State::Completed { outcome };
                return Err(NewInputError::Completed);
            }
            State::Active { turn, intent: Some(_) } => {
                self.withdrawals.push(Withdrawal { turn, reason: "new input withdrew requested finish".to_owned() })
            }
            State::Ready { outcome }
            | State::Attention { outcome: Some(outcome), .. }
            | State::Closing { outcome, .. } => {
                self.archive.push(Archived { outcome, reason: "new input invalidated prior result".to_owned() })
            }
            _ => {}
        }
        self.state = State::Active { turn, intent: None };
        self.applied(event, "new_input", facts);
        Ok(())
    }

    pub fn request_finish(&mut self, event: EventId, turn: &Turn) -> Result<(), FinishError> {
        let facts = facts(&[turn.as_str()]);
        if self.replayed(&event, "request_finish", &facts).map_err(FinishError::Event)? {
            return Ok(());
        }
        let Some(active) = self.state.turn() else { return Err(FinishError::NotActive) };
        if active != turn {
            return Err(FinishError::TurnMismatch);
        }
        self.state = State::Active { turn: turn.clone(), intent: Some(Intent { turn: turn.clone() }) };
        self.applied(event, "request_finish", facts);
        Ok(())
    }

    pub fn wait(&mut self, event: EventId, turn: &Turn, reason: WaitReason) -> Result<(), WaitError> {
        let facts = facts(&[turn.as_str(), reason.description(), &format!("{:?}", reason.kind())]);
        if self.replayed(&event, "wait", &facts).map_err(WaitError::Event)? {
            return Ok(());
        }
        let State::Active { turn: active, intent } = &self.state else { return Err(WaitError::NotActive) };
        if active != turn {
            return Err(WaitError::TurnMismatch);
        }
        if intent.is_some() {
            self.withdrawals
                .push(Withdrawal { turn: turn.clone(), reason: "explicit wait withdrew requested finish".to_owned() });
        }
        self.state = State::Waiting { turn: turn.clone(), reason, report: None, finish_requested: false };
        self.applied(event, "wait", facts);
        Ok(())
    }

    pub fn set_hold(&mut self, event: EventId) -> Result<(), HoldError> {
        self.change_hold(event, true, "set_hold")
    }
    pub fn release_hold(&mut self, event: EventId) -> Result<(), HoldError> {
        self.change_hold(event, false, "release_hold")
    }

    fn change_hold(&mut self, event: EventId, held: bool, operation: &'static str) -> Result<(), HoldError> {
        let facts = facts(&[&held.to_string()]);
        if self.replayed(&event, operation, &facts).map_err(HoldError::Event)? {
            return Ok(());
        }
        if matches!(self.state, State::Completed { .. }) {
            return Err(HoldError::Completed);
        }
        self.hold = held;
        if held && matches!(self.state, State::Closing { .. }) {
            if let State::Closing { outcome, .. } = std::mem::replace(&mut self.state, State::Inactive) {
                self.state = State::Attention {
                    reason: "keep-open hold invalidated closing token".to_owned(),
                    report: None,
                    outcome: Some(outcome),
                };
            }
        }
        self.applied(event, operation, facts);
        Ok(())
    }

    /// Records a native end. A nonempty `pending` fact retains the report but cannot make a result ready.
    pub fn end_turn(
        &mut self,
        event: EventId,
        turn: &Turn,
        report: Option<Report>,
        pending: Option<WaitReason>,
    ) -> Result<(), EndTurnError> {
        let facts = facts(&[
            turn.as_str(),
            report.as_ref().map_or("", Report::as_str),
            pending.as_ref().map_or("", WaitReason::description),
            pending.as_ref().map_or("", |reason| match reason.kind() {
                WaitKind::User => "user",
                WaitKind::Child => "child",
                WaitKind::Ci => "ci",
                WaitKind::Approval => "approval",
            }),
        ]);
        if self.replayed(&event, "end_turn", &facts).map_err(EndTurnError::Event)? {
            return Ok(());
        }
        let next = match &self.state {
            State::Active { turn: active, intent: Some(intent) } if active == turn && intent.turn == *turn => {
                match pending {
                    Some(reason) => State::Waiting { turn: turn.clone(), reason, report, finish_requested: true },
                    None => {
                        let report = report.ok_or(EndTurnError::MissingReport)?;
                        State::Ready {
                            outcome: Outcome {
                                turn: turn.clone(),
                                report,
                                receipt: ResultReceipt { event: event.clone() },
                                outbox: Outbox::new(),
                            },
                        }
                    }
                }
            }
            State::Active { turn: active, intent: None } if active == turn => match pending {
                Some(reason) => State::Waiting { turn: turn.clone(), reason, report, finish_requested: false },
                None => State::Attention {
                    reason: "turn ended without a requested finish".to_owned(),
                    report,
                    outcome: None,
                },
            },
            State::Waiting { turn: active, reason: existing_reason, report: existing_report, finish_requested }
                if active == turn =>
            {
                match pending {
                    Some(reason) => State::Waiting {
                        turn: turn.clone(),
                        reason,
                        report: report.or_else(|| existing_report.clone()),
                        finish_requested: *finish_requested,
                    },
                    None if *finish_requested => {
                        let report = report.or_else(|| existing_report.clone()).ok_or(EndTurnError::MissingReport)?;
                        State::Ready {
                            outcome: Outcome {
                                turn: turn.clone(),
                                report,
                                receipt: ResultReceipt { event: event.clone() },
                                outbox: Outbox::new(),
                            },
                        }
                    }
                    None => State::Waiting {
                        turn: turn.clone(),
                        reason: existing_reason.clone(),
                        report: report.or_else(|| existing_report.clone()),
                        finish_requested: false,
                    },
                }
            }
            State::Active { .. } | State::Waiting { .. } => return Err(EndTurnError::TurnMismatch),
            _ => return Err(EndTurnError::NotCurrent),
        };
        self.state = next;
        self.applied(event, "end_turn", facts);
        Ok(())
    }

    pub fn interrupt(&mut self, event: EventId, turn: &Turn, reason: &str) -> Result<(), InterruptError> {
        let facts = facts(&[turn.as_str(), reason]);
        if self.replayed(&event, "interrupt", &facts)? {
            return Ok(());
        }
        let current = self.state.turn().or_else(|| self.state.outcome().map(Outcome::turn));
        if current != Some(turn) || matches!(self.state, State::Completed { .. }) {
            return Err(InterruptError::TurnMismatch);
        }
        let report = match &self.state {
            State::Waiting { report, .. } => report.clone(),
            _ => self.state.outcome().map(|outcome| outcome.report().clone()),
        };
        if let Some(outcome) = self.state.outcome() {
            self.archive.push(Archived { outcome: outcome.clone(), reason: reason.to_owned() });
        }
        self.state = State::Attention { reason: reason.to_owned(), report, outcome: None };
        self.applied(event, "interrupt", facts);
        Ok(())
    }

    pub fn begin_close(&mut self, event: EventId, expected_revision: u64) -> Result<(), CloseError> {
        let facts = facts(&[&expected_revision.to_string()]);
        if self.replayed(&event, "begin_close", &facts).map_err(CloseError::Event)? {
            return Ok(());
        }
        if self.hold {
            return Err(CloseError::Held);
        }
        if self.revision != expected_revision {
            return Err(CloseError::Revision { expected: expected_revision, actual: self.revision });
        }
        let outcome = match std::mem::replace(&mut self.state, State::Inactive) {
            State::Ready { outcome } | State::Attention { outcome: Some(outcome), .. } => outcome,
            state => {
                self.state = state;
                return Err(CloseError::NotReady);
            }
        };
        let closing_revision = self.revision.checked_add(1).expect("completion revision cannot overflow");
        self.state = State::Closing { revision: closing_revision, outcome };
        self.applied(event, "begin_close", facts);
        Ok(())
    }

    pub fn record_delivery(
        &mut self,
        event: EventId,
        closing_revision: u64,
        receipt: Receipt,
    ) -> Result<(), DeliveryError> {
        if self.state.closing_revision().is_none() {
            let facts = facts(&[&closing_revision.to_string(), receipt.id()]);
            if self.replayed(&event, "record_delivery", &facts)? {
                return Ok(());
            }
            if self.revision != closing_revision {
                return Err(DeliveryError::ClosingRevision { expected: closing_revision });
            }
            let outcome = match &mut self.state {
                State::Ready { outcome } | State::Attention { outcome: Some(outcome), .. } => outcome,
                _ => return Err(DeliveryError::NotClosing),
            };
            if outcome.outbox.receipt(Step::Delivery).is_some() {
                return Err(DeliveryError::AlreadyRecorded(Step::Delivery));
            }
            outcome.outbox.delivery = StepState::Recorded(receipt);
            self.state = State::Ready { outcome: outcome.clone() };
            self.applied(event, "record_delivery", facts);
            return Ok(());
        }
        self.record_receipt(event, closing_revision, receipt, Step::Delivery, "record_delivery")
    }
    pub fn record_tracker(
        &mut self,
        event: EventId,
        closing_revision: u64,
        receipt: Receipt,
    ) -> Result<(), DeliveryError> {
        self.record_receipt(event, closing_revision, receipt, Step::Tracker, "record_tracker")
    }
    pub fn record_cleanup(
        &mut self,
        event: EventId,
        closing_revision: u64,
        receipt: Receipt,
    ) -> Result<(), DeliveryError> {
        self.record_receipt(event, closing_revision, receipt, Step::Cleanup, "record_cleanup")
    }

    fn record_receipt(
        &mut self,
        event: EventId,
        closing_revision: u64,
        receipt: Receipt,
        step: Step,
        operation: &'static str,
    ) -> Result<(), DeliveryError> {
        let facts = facts(&[&closing_revision.to_string(), receipt.id()]);
        if self.replayed(&event, operation, &facts).map_err(DeliveryError::Event)? {
            return Ok(());
        }
        let State::Closing { revision, outcome } = &mut self.state else { return Err(DeliveryError::NotClosing) };
        if *revision != closing_revision {
            return Err(DeliveryError::ClosingRevision { expected: closing_revision });
        }
        if matches!(outcome.outbox.step(step), StepState::Recorded(_)) {
            return Err(DeliveryError::AlreadyRecorded(step));
        }
        *outcome.outbox.step_mut(step) = StepState::Recorded(receipt);
        self.applied(event, operation, facts);
        Ok(())
    }

    pub fn fail(&mut self, event: EventId, closing_revision: u64, failure: Failure) -> Result<(), FailError> {
        let facts = facts(&[&closing_revision.to_string(), &format!("{:?}", failure.step()), failure.description()]);
        if self.replayed(&event, "fail", &facts).map_err(FailError::Event)? {
            return Ok(());
        }
        let closing = self.state.closing_revision();
        let revision = closing.unwrap_or(self.revision);
        if revision != closing_revision {
            return Err(FailError::ClosingRevision { expected: closing_revision });
        }
        let outcome = match &mut self.state {
            State::Closing { outcome, .. } => outcome,
            State::Ready { outcome } | State::Attention { outcome: Some(outcome), .. }
                if failure.step() == Step::Delivery =>
            {
                outcome
            }
            _ => return Err(FailError::NotClosing),
        };
        if matches!(outcome.outbox.step(failure.step()), StepState::Recorded(_)) {
            return Err(FailError::AlreadyRecorded(failure.step()));
        }
        if closing.is_none()
            && matches!(outcome.outbox.step(failure.step()), StepState::Failed(previous) if previous == &failure)
        {
            return Ok(());
        }
        *outcome.outbox.step_mut(failure.step()) = StepState::Failed(failure.clone());
        let outcome = outcome.clone();
        self.state = State::Attention {
            reason: format!("{:?} closeout failed: {}", failure.step(), failure.description()),
            report: None,
            outcome: Some(outcome),
        };
        self.applied(event, "fail", facts);
        Ok(())
    }

    pub fn complete(&mut self, event: EventId, closing_revision: u64) -> Result<(), CompleteError> {
        let facts = facts(&[&closing_revision.to_string()]);
        if self.replayed(&event, "complete", &facts).map_err(CompleteError::Event)? {
            return Ok(());
        }
        let State::Closing { revision, outcome } = &self.state else { return Err(CompleteError::NotClosing) };
        if *revision != closing_revision {
            return Err(CompleteError::ClosingRevision { expected: closing_revision });
        }
        if !outcome.outbox.complete() {
            return Err(CompleteError::PendingSteps);
        }
        let outcome = outcome.clone();
        self.state = State::Completed { outcome };
        self.applied(event, "complete", facts);
        Ok(())
    }

    fn replayed(&self, event: &EventId, operation: &'static str, facts: &str) -> Result<bool, EventError> {
        let Some(applied) = self.applied.iter().find(|applied| applied.id == *event) else { return Ok(false) };
        if applied.operation == operation && applied.facts == facts {
            Ok(true)
        } else {
            Err(EventError::Conflict(event.clone()))
        }
    }
    fn applied(&mut self, id: EventId, operation: &'static str, facts: String) {
        self.applied.push(Applied { id, operation, facts });
        self.revision = self.revision.checked_add(1).expect("completion revision cannot overflow");
    }

    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        storage::encode(self)
    }
}

fn facts(parts: &[&str]) -> String {
    parts.iter().fold(String::new(), |mut encoded, part| {
        encoded.push_str(&format!("{}:{part}", part.len()));
        encoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> EventId {
        EventId::new(value).unwrap()
    }
    fn turn(value: &str) -> Turn {
        Turn::new(value).unwrap()
    }
    fn receipt(value: &str) -> Receipt {
        Receipt::new(value).unwrap()
    }
    fn record() -> Record {
        Record::new(Identity::new("launch_1", "codex:thread_1").unwrap())
    }

    #[test]
    fn requested_finish_requires_the_current_turn_and_nonempty_report() {
        let mut record = record();
        record.new_input(id("input"), turn("one")).unwrap();
        assert!(matches!(record.request_finish(id("wrong"), &turn("two")), Err(FinishError::TurnMismatch)));
        record.request_finish(id("finish"), &turn("one")).unwrap();
        assert!(matches!(record.end_turn(id("end"), &turn("one"), None, None), Err(EndTurnError::MissingReport)));
        record.end_turn(id("end"), &turn("one"), Some(Report::new("done").unwrap()), None).unwrap();
        assert!(matches!(record.state(), State::Ready { .. }));
    }

    #[test]
    fn a_turn_without_finish_is_visible_attention_not_completion() {
        let mut record = record();
        record.new_input(id("input"), turn("one")).unwrap();
        record.end_turn(id("end"), &turn("one"), Some(Report::new("report").unwrap()), None).unwrap();
        assert!(
            matches!(record.state(), State::Attention { reason, .. } if reason == "turn ended without a requested finish")
        );
        assert!(record.state().outcome().is_none());
    }

    #[test]
    fn native_pending_retains_the_report_but_never_enables_close_until_a_verified_clear() {
        let mut record = record();
        record.new_input(id("input"), turn("one")).unwrap();
        record.request_finish(id("finish"), &turn("one")).unwrap();
        let report = Report::new("final response").unwrap();
        let pending = WaitReason::new(WaitKind::Child, "native child task remains").unwrap();
        record.end_turn(id("pending"), &turn("one"), Some(report.clone()), Some(pending)).unwrap();
        assert!(
            matches!(record.state(), State::Waiting { report: Some(saved), finish_requested: true, .. } if saved == &report)
        );
        assert!(matches!(record.begin_close(id("close"), record.revision()), Err(CloseError::NotReady)));
        record.end_turn(id("clear"), &turn("one"), None, None).unwrap();
        assert!(matches!(record.state(), State::Ready { outcome } if outcome.report() == &report));
    }

    #[test]
    fn wait_suppresses_finish_and_a_new_turn_clears_only_the_wait() {
        let mut record = record();
        record.set_hold(id("hold")).unwrap();
        record.new_input(id("input"), turn("one")).unwrap();
        record.request_finish(id("finish"), &turn("one")).unwrap();
        record
            .wait(id("wait"), &turn("one"), WaitReason::new(WaitKind::Approval, "approval requested").unwrap())
            .unwrap();
        record.end_turn(id("end"), &turn("one"), Some(Report::new("report").unwrap()), None).unwrap();
        assert!(matches!(record.state(), State::Waiting { .. }));
        record.new_input(id("next"), turn("two")).unwrap();
        assert!(matches!(record.state(), State::Active { .. }));
        assert!(record.held_open());
    }

    #[test]
    fn new_input_archives_a_delivered_result_and_invalidates_closing() {
        let mut record = ready();
        let revision = record.revision();
        record.begin_close(id("close"), revision).unwrap();
        assert!(record.state().closing_revision().is_some());
        record.new_input(id("next"), turn("two")).unwrap();
        assert!(matches!(record.state(), State::Active { .. }));
        assert_eq!(record.archive.len(), 1);
    }

    #[test]
    fn close_requires_every_explicit_external_receipt_and_keeps_failures_retryable() {
        let mut record = ready();
        record.begin_close(id("close"), record.revision()).unwrap();
        let closing = record.state().closing_revision().unwrap();
        record.record_delivery(id("delivery"), closing, receipt("delivery")).unwrap();
        record
            .fail(id("tracker-failed"), closing, Failure::new(Step::Tracker, "tracker unavailable").unwrap())
            .unwrap();
        assert!(
            matches!(record.state(), State::Attention { reason, .. } if reason == "Tracker closeout failed: tracker unavailable")
        );
        record.begin_close(id("retry-close"), record.revision()).unwrap();
        let retry = record.state().closing_revision().unwrap();
        record.record_tracker(id("tracker"), retry, receipt("tracker")).unwrap();
        record.record_cleanup(id("cleanup"), retry, receipt("cleanup")).unwrap();
        record.complete(id("complete"), retry).unwrap();
        assert!(matches!(record.state(), State::Completed { .. }));
    }

    #[test]
    fn duplicate_events_are_idempotent_but_conflicting_retries_fail() {
        let mut record = record();
        record.new_input(id("input"), turn("one")).unwrap();
        let revision = record.revision();
        record.new_input(id("input"), turn("one")).unwrap();
        assert_eq!(record.revision(), revision);
        assert!(matches!(
            record.new_input(id("input"), turn("two")),
            Err(NewInputError::Event(EventError::Conflict(_)))
        ));
    }

    fn ready() -> Record {
        let mut record = record();
        record.new_input(id("input"), turn("one")).unwrap();
        record.request_finish(id("finish"), &turn("one")).unwrap();
        record.end_turn(id("end"), &turn("one"), Some(Report::new("final report").unwrap()), None).unwrap();
        record
    }

    #[test]
    fn holds_preserve_work_and_results_while_invalidating_only_closing() {
        let mut active = record();
        active.new_input(id("input"), turn("turn")).unwrap();
        active.set_hold(id("hold")).unwrap();
        assert!(matches!(active.state(), State::Active { .. }));
        active.request_finish(id("finish"), &turn("turn")).unwrap();
        active.end_turn(id("end"), &turn("turn"), Some(Report::new("result").unwrap()), None).unwrap();
        active.record_delivery(id("delivery"), active.revision(), receipt("parent delivery stored")).unwrap();
        assert!(matches!(active.begin_close(id("close"), active.revision()), Err(CloseError::Held)));
        active.release_hold(id("release")).unwrap();
        active.begin_close(id("close"), active.revision()).unwrap();
        let old = active.state().closing_revision().unwrap();
        active.set_hold(id("hold-again")).unwrap();
        assert!(active.state().closing_revision().is_none());
        assert!(active.complete(id("stale-complete"), old).is_err());
        assert_eq!(active.state().outcome().unwrap().report().as_str(), "result");
    }

    #[test]
    fn delivery_failure_is_durable_before_close_without_clearing_the_hold_or_repeating_events() {
        let mut record = ready();
        record.set_hold(id("hold")).unwrap();
        let failure = Failure::new(Step::Delivery, "native permission refused").unwrap();
        record.fail(id("failure"), record.revision(), failure.clone()).unwrap();
        assert!(record.held_open());
        assert!(matches!(record.state(), State::Attention { outcome: Some(_), .. }));
        let failed = record.clone();
        record.fail(id("same-failure"), record.revision(), failure.clone()).unwrap();
        assert_eq!(record, failed);
        record.record_delivery(id("delivered"), record.revision(), receipt("submitted")).unwrap();
        assert!(record.begin_close(id("still-held"), record.revision()).is_err());
        let revision = record.revision();
        record.new_input(id("new-turn"), turn("two")).unwrap();
        assert!(record.fail(id("stale-failure"), revision, failure).is_err());
        assert!(matches!(record.state(), State::Active { .. }));
    }

    #[test]
    fn native_interruption_invalidates_result_without_authorizing_a_retry() {
        let mut record = ready();
        record.begin_close(id("close"), record.revision()).unwrap();
        let closing = record.state().closing_revision().unwrap();
        record.interrupt(id("interrupt"), &turn("one"), "user interrupted").unwrap();
        assert!(matches!(record.state(), State::Attention { reason, .. } if reason == "user interrupted"));
        assert!(record.complete(id("complete"), closing).is_err());
        assert!(record.begin_close(id("retry"), record.revision()).is_err());
        assert_eq!(record.archive.len(), 1);
    }
}
