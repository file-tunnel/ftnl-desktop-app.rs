//! Pure, operation-correlated receive lifecycle for every desktop UI effect.

use ftnl_ui_components::picker_machine::{
    InvalidPickerTransition, PickerMachineEvent, PickerMachineState,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationId(u64);

impl OperationId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Start,
    Refresh,
    Download,
    Cancel,
    Retry,
    Reset,
}

impl Command {
    const fn event(self) -> PickerMachineEvent {
        match self {
            Self::Start => PickerMachineEvent::Start,
            Self::Refresh => PickerMachineEvent::Refresh,
            Self::Download => PickerMachineEvent::Download,
            Self::Cancel => PickerMachineEvent::Cancel,
            Self::Retry => PickerMachineEvent::Retry,
            Self::Reset => PickerMachineEvent::Reset,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Created,
    SnapshotEmpty,
    SnapshotFiles,
    AllFilesReceived,
    Downloaded,
    Cancelled,
    Failed,
}

impl Completion {
    const fn event(self) -> PickerMachineEvent {
        match self {
            Self::Created => PickerMachineEvent::Created,
            Self::SnapshotEmpty => PickerMachineEvent::SnapshotEmpty,
            Self::SnapshotFiles => PickerMachineEvent::SnapshotFiles,
            Self::AllFilesReceived => PickerMachineEvent::AllFilesReceived,
            Self::Downloaded => PickerMachineEvent::Downloaded,
            Self::Cancelled => PickerMachineEvent::Cancelled,
            Self::Failed => PickerMachineEvent::Fail,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectKind {
    Create,
    Refresh,
    Download,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Effect {
    pub operation: OperationId,
    pub kind: EffectKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LifecycleError {
    #[error("picker event is not allowed in the current state")]
    InvalidTransition,
    #[error("worker response does not match the active operation")]
    StaleOperation,
    #[error("state-machine effect has no controlled worker operation")]
    UncontrolledEffect,
    #[error("state-machine in-flight metadata disagrees with operation ownership")]
    InFlightInvariant,
    #[error("state-machine session metadata disagrees with session ownership")]
    SessionInvariant,
}

impl From<InvalidPickerTransition> for LifecycleError {
    fn from(_: InvalidPickerTransition) -> Self {
        Self::InvalidTransition
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lifecycle {
    state: PickerMachineState,
    active_operation: Option<OperationId>,
    next_operation: u64,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            state: PickerMachineState::Idle,
            active_operation: None,
            next_operation: 1,
        }
    }
}

impl Lifecycle {
    pub const fn state(&self) -> PickerMachineState {
        self.state
    }

    pub const fn active_operation(&self) -> Option<OperationId> {
        self.active_operation
    }

    pub const fn is_in_flight(&self) -> bool {
        self.active_operation.is_some()
    }

    pub fn can(&self, command: Command) -> bool {
        self.state.transition(command.event()).is_ok()
    }

    pub fn dispatch(&mut self, command: Command) -> Result<Option<Effect>, LifecycleError> {
        let next = self.state.transition(command.event())?;
        let effect_kind = effect_kind(next)?;
        let effect = effect_kind.map(|kind| Effect {
            operation: self.allocate_operation(),
            kind,
        });

        self.state = next;
        self.active_operation = effect.map(|item| item.operation);
        self.validate_in_flight()?;
        Ok(effect)
    }

    pub fn complete(
        &mut self,
        operation: OperationId,
        completion: Completion,
    ) -> Result<(), LifecycleError> {
        if self.active_operation != Some(operation) {
            return Err(LifecycleError::StaleOperation);
        }
        let next = self.state.transition(completion.event())?;
        if next.metadata().in_flight {
            return Err(LifecycleError::UncontrolledEffect);
        }

        self.state = next;
        self.active_operation = None;
        self.validate_in_flight()
    }

    pub fn validate_session(&self, has_session: bool) -> Result<(), LifecycleError> {
        if self.state.metadata().requires_session == has_session {
            Ok(())
        } else {
            Err(LifecycleError::SessionInvariant)
        }
    }

    fn allocate_operation(&mut self) -> OperationId {
        let operation = OperationId(self.next_operation);
        self.next_operation = self.next_operation.checked_add(1).unwrap_or(1);
        operation
    }

    fn validate_in_flight(&self) -> Result<(), LifecycleError> {
        if self.state.metadata().in_flight == self.active_operation.is_some() {
            Ok(())
        } else {
            Err(LifecycleError::InFlightInvariant)
        }
    }
}

fn effect_kind(state: PickerMachineState) -> Result<Option<EffectKind>, LifecycleError> {
    let effect = match state {
        PickerMachineState::Creating => Some(EffectKind::Create),
        PickerMachineState::Refreshing => Some(EffectKind::Refresh),
        PickerMachineState::Downloading => Some(EffectKind::Download),
        PickerMachineState::Cancelling => Some(EffectKind::Cancel),
        _ if state.metadata().in_flight => return Err(LifecycleError::UncontrolledEffect),
        _ => None,
    };
    Ok(effect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn require_effect(machine: &mut Lifecycle, command: Command) -> Effect {
        machine
            .dispatch(command)
            .expect("command should be legal")
            .expect("command should start one controlled effect")
    }

    #[test]
    fn happy_path_preserves_session_and_operation_invariants() {
        let mut machine = Lifecycle::default();
        machine.validate_session(false).unwrap();

        let create = require_effect(&mut machine, Command::Start);
        assert_eq!(create.kind, EffectKind::Create);
        machine.validate_session(false).unwrap();
        machine
            .complete(create.operation, Completion::Created)
            .unwrap();
        machine.validate_session(true).unwrap();

        let refresh = require_effect(&mut machine, Command::Refresh);
        assert_eq!(refresh.kind, EffectKind::Refresh);
        machine
            .complete(refresh.operation, Completion::SnapshotFiles)
            .unwrap();
        machine.validate_session(true).unwrap();

        let download = require_effect(&mut machine, Command::Download);
        assert_eq!(download.kind, EffectKind::Download);
        machine
            .complete(download.operation, Completion::Downloaded)
            .unwrap();
        machine.validate_session(true).unwrap();
    }

    #[test]
    fn stale_and_duplicate_responses_stutter() {
        let mut machine = Lifecycle::default();
        let create = require_effect(&mut machine, Command::Start);
        let before = machine.clone();
        assert_eq!(
            machine.complete(OperationId(create.operation.get() + 1), Completion::Created),
            Err(LifecycleError::StaleOperation)
        );
        assert_eq!(machine, before);

        machine
            .complete(create.operation, Completion::Created)
            .unwrap();
        let after = machine.clone();
        assert_eq!(
            machine.complete(create.operation, Completion::Created),
            Err(LifecycleError::StaleOperation)
        );
        assert_eq!(machine, after);
    }

    #[test]
    fn illegal_commands_are_rejected_without_mutation() {
        let mut machine = Lifecycle::default();
        let before = machine.clone();
        assert_eq!(
            machine.dispatch(Command::Download),
            Err(LifecycleError::InvalidTransition)
        );
        assert_eq!(machine, before);
    }

    #[test]
    fn retry_is_deterministic_for_session_ownership() {
        let mut without_session = Lifecycle::default();
        let create = require_effect(&mut without_session, Command::Start);
        without_session
            .complete(create.operation, Completion::Failed)
            .unwrap();
        without_session.validate_session(false).unwrap();
        assert_eq!(
            require_effect(&mut without_session, Command::Retry).kind,
            EffectKind::Create
        );

        let mut with_session = Lifecycle::default();
        let create = require_effect(&mut with_session, Command::Start);
        with_session
            .complete(create.operation, Completion::Created)
            .unwrap();
        let refresh = require_effect(&mut with_session, Command::Refresh);
        with_session
            .complete(refresh.operation, Completion::Failed)
            .unwrap();
        with_session.validate_session(true).unwrap();
        assert_eq!(
            require_effect(&mut with_session, Command::Retry).kind,
            EffectKind::Refresh
        );
    }
}
