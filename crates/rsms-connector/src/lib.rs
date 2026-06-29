//! TCP 连接器

//! - **服务端**：`ServerBuilder::serve()` 启动监听，`BoundServer` 管理接受循环和连接池
//! - **客户端**：`ClientBuilder::connect()` 连接到服务器，`ClientConnection` 管理单条连接
//!
//! 单连接处理顺序：**帧解码 → CMPP 消息解码 → 会话占位 → 业务链**（见 `run_connection`）。

pub mod adapter_registry;
pub mod client;
pub mod connection;
pub mod handlers;
pub mod id_generator;
pub mod pool;
pub mod protocol;
pub mod server;
pub mod transaction;

pub use client::{
    ClientBuilder, ClientConnection,
    CmppDecoder, SgipDecoder, SmgpDecoder, SmppDecoder,
    ClientConfig, ConnectionEvent,
};
pub use connection::Connection;
pub use handlers::cmpp::CmppHandler;
pub use handlers::sgip::SgipHandler;
pub use handlers::smgp::SmgpHandler;
pub use handlers::smpp::SmppHandler;
pub use pool::{ConnectionPool, AccountPool, AccountConnections};
pub use id_generator::SimpleIdGenerator;
pub use rsms_core::IdGenerator;
pub use rsms_core::Protocol;
pub use rsms_core::{ErrorKind, Metrics, NoopMetrics};
pub use protocol::{
    ProtocolHandler, ProtocolConnection, AuthHandler, AuthCredentials, AuthResult,
    AccountConfig, AccountPoolConfig, AccountConfigProvider,
    MessageSource, MessageItem, FrameDecoder,
    ServerEventHandler, ClientEventHandler,
};
pub use server::{ServerBuilder, BoundServer, ServerShutdown};
pub use transaction::{
    TransactionManager, TransactionStatus, MessageCallback, SubmitInfo, ReportInfo, MoInfo,
    cmpp::{CmppSubmit, CmppDeliver, CmppTransactionManager},
    smgp::{SmgpSubmit, SmgpDeliver, SmgpTransactionManager},
    sgip::{SgipSubmit, SgipDeliver, SgipTransactionManager},
    smpp::{SmppSubmit, SmppDeliver, SmppTransactionManager},
};
