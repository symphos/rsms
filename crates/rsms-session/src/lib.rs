//! 会话状态与完整会话管理器（对齐 `AbstractSessionStateManager`）。
use parking_lot::{Mutex, RwLock};
use rsms_core::{ConnectionInfo, EndpointConfig, SessionState};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod state_machine;
pub use state_machine::{SessionStateMachine, StateTransitionError};

pub mod heartbeat;
pub use heartbeat::{HeartbeatConfig, HeartbeatTimer};

/// 每连接会话上下文（对齐 `Channel.attr` 中的 session / endpoint）。
#[derive(Debug)]
pub struct ConnectionContext {
    pub endpoint: Arc<EndpointConfig>,
    pub session_state: RwLock<SessionState>,
    pub manager: SessionManager,
    state_machine: RwLock<SessionStateMachine>,
    authenticated_account: Mutex<Option<String>>,
    pub last_activity: Mutex<Instant>,
    protocol_version: Mutex<Option<u8>>,
    pub connection_info: ConnectionInfo,
}

impl ConnectionContext {
    pub fn new(endpoint: Arc<EndpointConfig>) -> Self {
        Self {
            endpoint,
            session_state: RwLock::new(SessionState::Disconnected),
            manager: SessionManager::new(),
            state_machine: RwLock::new(SessionStateMachine::new()),
            authenticated_account: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            protocol_version: Mutex::new(None),
            connection_info: ConnectionInfo::unknown(),
        }
    }

    pub fn with_connection_info(endpoint: Arc<EndpointConfig>, info: ConnectionInfo) -> Self {
        Self {
            endpoint,
            session_state: RwLock::new(SessionState::Disconnected),
            manager: SessionManager::new(),
            state_machine: RwLock::new(SessionStateMachine::new()),
            authenticated_account: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            protocol_version: Mutex::new(None),
            connection_info: info,
        }
    }

    pub fn mark_connected(&self) -> Result<(), StateTransitionError> {
        let mut sm = self.state_machine.write();
        sm.transition_to_connected()?;
        *self.session_state.write() = SessionState::Connected;
        *self.last_activity.lock() = Instant::now();
        Ok(())
    }

    pub fn mark_disconnected(&self) {
        let mut sm = self.state_machine.write();
        let _ = sm.transition_to_disconnected();
        *self.session_state.write() = SessionState::Disconnected;
        *self.authenticated_account.lock() = None;
    }

    pub fn mark_authenticated(&self, account: String) -> Result<(), StateTransitionError> {
        let mut sm = self.state_machine.write();
        sm.transition_to_authenticated()?;
        *self.authenticated_account.lock() = Some(account);
        *self.session_state.write() = SessionState::Logined;
        *self.last_activity.lock() = Instant::now();
        Ok(())
    }

    pub fn mark_reconnecting(&self) -> Result<(), StateTransitionError> {
        let mut sm = self.state_machine.write();
        sm.transition_to_reconnecting()
    }

    pub fn authenticated_account(&self) -> Option<String> {
        self.authenticated_account.lock().clone()
    }

    pub fn set_authenticated_account(&self, account: String) {
        *self.authenticated_account.lock() = Some(account);
    }

    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
    }

    pub fn last_activity(&self) -> Instant {
        *self.last_activity.lock()
    }

    pub fn is_idle(&self, idle_timeout: Duration) -> bool {
        self.last_activity.lock().elapsed() > idle_timeout
    }

    pub fn current_phase(&self) -> state_machine::SessionPhase {
        self.state_machine.read().current()
    }

    pub fn can_send_business(&self) -> bool {
        self.state_machine.read().can_send_business()
    }

    pub fn session_state(&self) -> SessionState {
        *self.session_state.read()
    }

    pub fn protocol_version(&self) -> Option<u8> {
        *self.protocol_version.lock()
    }

    pub fn set_protocol_version(&self, version: u8) {
        *self.protocol_version.lock() = Some(version);
    }
}

/// 会话管理器；后续可扩展登录、心跳、重试队列等。
#[derive(Debug, Default, Clone)]
pub struct SessionManager {
    _private: (),
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }
}
