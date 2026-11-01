# 技术方案

## 架构

仓库用 GitHub Actions 做检查和发布。版本决策集中在 `scripts/resolve-release.sh`，工作流和测试调用同一份逻辑。

```mermaid
flowchart TD
  pr[Pull Request] --> ci[持续集成]
  pushMaster[推送 master] --> ci
  pushMaster --> plan[解析版本]
  plan -->|跳过| skip[保留现有发布]
  plan -->|版本更高| verify[格式检查与测试]
  tagPush[推送版本标签] --> verify
  manual[手动运行 Release] --> verify
  verify --> release[创建 GitHub Release]
  release --> assets[上传五个平台的压缩包和校验文件]
```

持续集成定义在 `.github/workflows/ci.yml`，具体命令在 `.github/actions/rust-ci`。Release 工作流在决定发布之后调用同一个动作，避免两套检查漂移。创建 Release 和上传产物的任务显式要求前一个任务成功；带 `if` 的任务不会在依赖失败或跳过时继续执行。

## 技术栈

- Rust stable，edition 2024，`rust-toolchain.toml` 固定渠道。命令行只使用标准库。
- GitHub Actions：`actions/checkout@v7`、`dtolnay/rust-toolchain@stable`、`Swatinem/rust-cache@v2`、`softprops/action-gh-release@v3`、`taiki-e/upload-rust-binary-action@v1`。
- Dependabot 每周检查 Actions 和 Cargo 依赖更新。

`Cargo.toml` 设置 `publish = false`。发布目标是 GitHub Releases，不向 crates.io 上传。

## 版本决策

`scripts/resolve-release.sh` 只接受不含前导零的 `MAJOR.MINOR.PATCH`。

| 事件 | 条件 | 结果 |
| --- | --- | --- |
| 推送 `master` | 还没有稳定标签 | 跳过，并提示手动发布第一次 |
| 推送 `master` | `version` 高于最新稳定标签 | 发布 `v<version>` |
| 推送 `master` | `version` 等于最新稳定标签，且该标签指向当前提交 | 重新上传该 Release 的产物 |
| 推送 `master` | `version` 不高于最新稳定标签，且标签不指向当前提交 | 跳过 |
| 推送标签 | 标签与 `version` 一致 | 发布该标签 |
| 推送标签 | 标签与 `version` 不一致，或不是稳定版本号 | 失败 |
| 手动运行 | 分支是 `master`，标签为空或等于 `v<version>` | 发布 |
| 手动运行 | 分支不是 `master`，或标签与版本不一致 | 失败 |

准备发布的标签如果已经指向其他提交，脚本失败，工作流不创建 Release，因此不会移动标签。版本与最新标签相同、但标签指向更早的提交时，推送 `master` 会跳过。`master` 上的自动发布由同一次工作流创建标签并上传产物；用默认 `GITHUB_TOKEN` 创建的标签不会再次触发工作流。

## 发布产物

构建矩阵：

| target | 运行器 | 压缩格式 |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | `.tar.gz` |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-latest` | `.zip` |
| `aarch64-apple-darwin` | `macos-latest` | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-latest` | `.tar.gz` |

压缩包带前导目录，包含可执行文件、`LICENSE` 和 `README.md`。每个压缩包另有 SHA256 校验文件。Release 说明由 GitHub 根据提交自动生成，并标为最新版本。

## 权限

- CI 只有 `contents: read`。
- 创建 Release 的任务有 `contents: write` 和 `pull-requests: read`。
- 上传产物的任务有 `contents: write`。
- 令牌使用运行自带的 `GITHUB_TOKEN`，不读取额外密钥。

## 测试策略

- `cargo test` 覆盖参数解析、版本号格式，以及 `--version`、`--help` 和错误退出码。
- `scripts/resolve-release-test.sh` 覆盖决策表，并用临时 Git 仓库覆盖“选择最新标签”“第一次跳过”“标签指向其他提交”。
- 本地用 shellcheck 和 actionlint 检查脚本与工作流。
- 多平台上传依赖 GitHub 托管运行器，本地不执行实际上传。

## 安全性

- 进入标签和版本比较的字符串先经正则校验，再写入工作流输出。
- 发布任务不移动已经指向其他提交的标签。
- Release 构建使用 `--locked`，并提交 `Cargo.lock`。
- CI 的 `RUSTFLAGS=-Dwarnings` 只加在检查任务上，避免上传任务在安装交叉编译工具时把警告当成错误。
