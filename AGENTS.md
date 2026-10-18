# Nuono Registry 开发约定

- 工具统一使用 Rust 和 Cargo；不引入 Node.js、npm 或其他语言的开发脚本。工具链由 `rust-toolchain.toml` 固定，提交 `Cargo.lock`，CI 使用 `--locked`。
- 本地检查：`cargo fmt --all -- --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`。
- 目录检查和生成：`cargo run --locked -- validate`、`cargo run --locked -- build`。`dist/` 是可重建产物，不提交到 main。
- `catalog.json` 保留只含业务插件的 `schema_version: 1`；`extensions-v2.json` 使用 `schema_version: 2` 和带 kind 的 `extensions`。二者从同一份 entries 生成；`release_tag` 仅用于更新检测，不输出到目录。签名消息必须保持 `repo + "\n" + commit`，不改客户端契约。
- 投稿与更新只生成待审核 PR。候选 PR 不得通过修改 framework 锁定信息绕过受信任宿主的构建验证，不绕过 main 的审核规则。
- 发布凭据只交给已编译的发布工具；Cargo 编译、测试和外部插件构建不接收发布密钥或写入 token，不复用不可信构建缓存。
- 示例和伪造登记仅放测试临时目录，不作为生产条目收录。`catalog` 分支由校验通过的 main 自动发布，不手动维护。
