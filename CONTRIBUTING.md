# 贡献指南

感谢你对 RSMS 项目的关注！本文档说明如何参与贡献。

## 分支规范

### 长期分支

| 分支 | 用途 |
|------|------|
| `main` | 稳定发布，所有功能经过测试后合并 |
| `develop` | 开发集成，下一个版本的功能汇总 |

### 临时分支

| 类型 | 命名格式 | 示例 |
|------|---------|------|
| 功能开发 | `feature/<名称>` | `feature/longmsg-tlv-optimize` |
| Bug 修复 | `fix/<描述>` | `fix/smgp-tlv-tag-mismatch` |
| 实验性探索 | `explore/<内容>` | `explore/smpp-v50-stress-test` |

### 版本标签

```
v<主版本>.<次版本>.<补丁版本>[-<预发布标识>]

示例：
v0.1.0          — 首个正式发布
v0.2.0          — 新增功能
v0.1.1          — Bug 修复
v0.2.0-alpha    — 预发布版本
```

## 贡献流程

### 1. Fork 并创建分支

```bash
# Fork 后克隆你的仓库
git clone git@github.com:<你的用户名>/rsms.git
cd rsms

# 从 develop 创建功能分支
git checkout develop
git pull origin develop
git checkout -b feature/your-feature
```

### 2. 开发与测试

```bash
# 编译检查
cargo build --workspace

# 运行单元测试
cargo test --workspace --lib

# 运行集成测试（按协议）
cargo test -p cmpp-endpoint-example --test cmpp-integration-tests
cargo test -p smgp-endpoint-example --test smgp-integration-tests
cargo test -p smpp-endpoint-example --test smpp-integration-tests
cargo test -p sgip-endpoint-example --test sgip-integration-tests

# 运行长短信测试
cargo test -p cmpp-endpoint-example --test cmpp-longmsg-test
```

### 3. 提交代码

```bash
git add .
git commit -m "feat: 简要描述改动内容"
git push origin feature/your-feature
```

### 4. 提交 Pull Request

- 目标分支：`develop`
- 标题格式：`<类型>: <简要描述>`
- 类型参考：
  - `feat` — 新功能
  - `fix` — Bug 修复
  - `refactor` — 重构
  - `docs` — 文档更新
  - `test` — 测试相关
  - `chore` — 构建/配置变更

## 代码规范

- 遵循 Rust 惯用写法（`cargo clippy` 无警告）
- 公开 API 必须有文档注释（`///` 或 `//!`）
- 提交前确保 `cargo build --workspace` 和 `cargo test --workspace --lib` 通过
- 不要提交 `target/`、IDE 配置、日志文件等

## 项目结构

```
crates/              — 核心框架 crate
  rsms-core/         — 基础类型
  rsms-connector/    — 连接管理（服务端/客户端）
  rsms-business/     — 业务处理器 trait
  rsms-codec-cmpp/   — CMPP 编解码
  rsms-codec-smgp/   — SMGP 编解码
  rsms-codec-smpp/   — SMPP 编解码
  rsms-codec-sgip/   — SGIP 编解码
  rsms-longmsg/      — 长短信拆分/合包
  rsms-window/       — 滑动窗口
  rsms-ratelimit/    — 令牌桶限流
  rsms-session/      — 连接状态管理
  rsms-pipeline/     — 处理管道

examples/            — 示例代码
  cmpp-endpoint/     — CMPP 压测套件
  smgp-endpoint/     — SMGP 压测套件
  sgip-endpoint/     — SGIP 压测套件
  smpp-endpoint/     — SMPP 压测套件
  cmpp_server/       — CMPP 完整服务端示例
  cmpp_client/       — CMPP 完整客户端示例
  ... (四协议各 server/client)

docs/                — 文档
```

## License

提交代码即表示你同意以 Apache-2.0 许可证授权。
