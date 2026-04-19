//! Session state machine with validated state transitions.

use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Disconnected,
    Connecting,
    AwaitingLoginResp,
    Authenticated,
    Reconnecting,
}

impl fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionPhase::Disconnected => write!(f, "Disconnected"),
            SessionPhase::Connecting => write!(f, "Connecting"),
            SessionPhase::AwaitingLoginResp => write!(f, "AwaitingLoginResp"),
            SessionPhase::Authenticated => write!(f, "Authenticated"),
            SessionPhase::Reconnecting => write!(f, "Reconnecting"),
        }
    }
}

#[derive(Debug, Error)]
pub enum StateTransitionError {
    #[error("invalid transition from {from} to {to}")]
    InvalidTransition {
        from: SessionPhase,
        to: SessionPhase,
    },
}

#[derive(Debug, Clone)]
pub struct SessionStateMachine {
    current: SessionPhase,
}

impl SessionStateMachine {
    pub fn new() -> Self {
        Self {
            current: SessionPhase::Disconnected,
        }
    }

    pub fn current(&self) -> SessionPhase {
        self.current
    }

    pub fn transition_to_connected(&mut self) -> Result<(), StateTransitionError> {
        match self.current {
            SessionPhase::Disconnected => {
                self.current = SessionPhase::Connecting;
                Ok(())
            }
            SessionPhase::Reconnecting => {
                self.current = SessionPhase::Connecting;
                Ok(())
            }
            _ => Err(StateTransitionError::InvalidTransition {
                from: self.current,
                to: SessionPhase::Connecting,
            }),
        }
    }

    pub fn transition_to_awaiting_login(&mut self) -> Result<(), StateTransitionError> {
        match self.current {
            SessionPhase::Connecting => {
                self.current = SessionPhase::AwaitingLoginResp;
                Ok(())
            }
            _ => Err(StateTransitionError::InvalidTransition {
                from: self.current,
                to: SessionPhase::AwaitingLoginResp,
            }),
        }
    }

    pub fn transition_to_authenticated(&mut self) -> Result<(), StateTransitionError> {
        match self.current {
            SessionPhase::Connecting => {
                self.current = SessionPhase::Authenticated;
                Ok(())
            }
            SessionPhase::AwaitingLoginResp => {
                self.current = SessionPhase::Authenticated;
                Ok(())
            }
            _ => Err(StateTransitionError::InvalidTransition {
                from: self.current,
                to: SessionPhase::Authenticated,
            }),
        }
    }

    pub fn transition_to_disconnected(&mut self) -> Result<(), StateTransitionError> {
        self.current = SessionPhase::Disconnected;
        Ok(())
    }

    pub fn transition_to_reconnecting(&mut self) -> Result<(), StateTransitionError> {
        match self.current {
            SessionPhase::Authenticated => {
                self.current = SessionPhase::Reconnecting;
                Ok(())
            }
            SessionPhase::Connecting => {
                self.current = SessionPhase::Reconnecting;
                Ok(())
            }
            _ => Err(StateTransitionError::InvalidTransition {
                from: self.current,
                to: SessionPhase::Reconnecting,
            }),
        }
    }

    pub fn can_send_business(&self) -> bool {
        matches!(self.current, SessionPhase::Authenticated)
    }

    pub fn can_receive(&self) -> bool {
        !matches!(self.current, SessionPhase::Disconnected)
    }
}

impl Default for SessionStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut sm = SessionStateMachine::new();
        assert_eq!(sm.current(), SessionPhase::Disconnected);

        sm.transition_to_connected().unwrap();
        assert_eq!(sm.current(), SessionPhase::Connecting);

        sm.transition_to_awaiting_login().unwrap();
        assert_eq!(sm.current(), SessionPhase::AwaitingLoginResp);

        sm.transition_to_authenticated().unwrap();
        assert_eq!(sm.current(), SessionPhase::Authenticated);

        assert!(sm.can_send_business());
        assert!(sm.can_receive());
    }

    #[test]
    fn test_disconnected_state() {
        let sm = SessionStateMachine::new();
        assert_eq!(sm.current(), SessionPhase::Disconnected);
        assert!(!sm.can_send_business());
        assert!(!sm.can_receive());
    }

    #[test]
    fn test_invalid_transition() {
        let mut sm = SessionStateMachine::new();

        let result = sm.transition_to_authenticated();
        assert!(result.is_err());
        assert_eq!(sm.current(), SessionPhase::Disconnected);
    }

    #[test]
    fn test_reconnecting_transition() {
        let mut sm = SessionStateMachine::new();

        sm.transition_to_connected().unwrap();
        sm.transition_to_authenticated().unwrap();

        sm.transition_to_reconnecting().unwrap();
        assert_eq!(sm.current(), SessionPhase::Reconnecting);

        sm.transition_to_connected().unwrap();
        assert_eq!(sm.current(), SessionPhase::Connecting);
    }
}
