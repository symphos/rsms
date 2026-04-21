//! TCP 连接器

//! - **服务端**：`serve()` 启动监听，`BoundServer` 管理接受循环和连接池
//! - **客户端**：`connect()` 连接到服务器，`ClientConnection` 管理单条连接
//! - **客户端池**：`ClientPool` 管理多个连接，支持动态调整和自动重连
//!
//! 单连接处理顺序：**帧解码 → CMPP 消息解码 → 会话占位 → 业务链**（见 `run_connection`）。

pub mod client;
pub mod client_pool;
pub mod connection;
pub mod handlers;
pub mod id_generator;
pub mod pool;
pub mod protocol;
pub mod server;
pub mod transaction;

pub use client::{
    connect, connect_with_pool, ClientConnection, ClientHandler, ClientContext,
    CmppDecoder, SgipDecoder, SmgpDecoder, SmppDecoder,
    ClientConfig, ConnectionEvent,
};
pub use client_pool::{ClientPool, ConnectionReadyCallback};
pub use connection::Connection;
pub use handlers::cmpp::CmppHandler;
pub use handlers::sgip::SgipHandler;
pub use handlers::smgp::SmgpHandler;
pub use handlers::smpp::SmppHandler;
pub use pool::{ConnectionPool, AccountPool, AccountConnections};
pub use id_generator::SimpleIdGenerator;
pub use rsms_core::IdGenerator;
pub use protocol::{
    ProtocolHandler, ProtocolConnection, AuthHandler, AuthCredentials, AuthResult,
    AccountConfig, AccountPoolConfig, AccountConfigProvider,
    MessageSource, MessageItem, FrameDecoder,
    ServerEventHandler, ClientEventHandler,
};
pub use server::{serve, BoundServer};
pub use transaction::{
    TransactionManager, TransactionStatus, MessageCallback, SubmitInfo, ReportInfo, MoInfo,
    cmpp::{CmppSubmit, CmppDeliver, CmppTransactionManager},
    smgp::{SmgpSubmit, SmgpDeliver, SmgpTransactionManager},
    sgip::{SgipSubmit, SgipDeliver, SgipTransactionManager},
    smpp::{SmppSubmit, SmppDeliver, SmppTransactionManager},
};
