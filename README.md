# arknights-auto-update

[![CI](https://github.com/ModerRAS/arknights-auto-update/actions/workflows/ci.yml/badge.svg)](https://github.com/ModerRAS/arknights-auto-update/actions/workflows/ci.yml)
[![Release](https://github.com/ModerRAS/arknights-auto-update/actions/workflows/release.yml/badge.svg)](https://github.com/ModerRAS/arknights-auto-update/actions/workflows/release.yml)

明日方舟自动更新工具。当前命令行可以查看帮助和版本。合并到 `master` 的更高版本会自动构建，并发布到 GitHub Releases。

本项目以 GNU GPL v3 授权，见 [LICENSE](LICENSE)。

## 命令

```bash
arknights-auto-update --help
arknights-auto-update --version
```

## 开发

需要 Rust stable，渠道写在 `rust-toolchain.toml`。

```bash
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
bash scripts/resolve-release-test.sh
```

Pull Request 和推送到 `master` 时，CI 会执行同样的检查。

## 自动发布

版本号以 `Cargo.toml` 的 `version` 为准，标签是 `v<version>`，例如 `0.1.0` 对应 `v0.1.0`。发布物是五个平台的压缩包和 SHA256 校验文件。压缩包内包含可执行文件、`LICENSE` 和 `README.md`。

| 文件名中的目标 | 平台 |
| --- | --- |
| `x86_64-unknown-linux-musl` | Linux x86_64，静态链接 |
| `aarch64-unknown-linux-musl` | Linux arm64，静态链接 |
| `x86_64-pc-windows-msvc` | Windows x86_64 |
| `aarch64-apple-darwin` | macOS Apple 芯片 |
| `x86_64-apple-darwin` | macOS Intel |

### 第一次发布

仓库还没有 `vX.Y.Z` 标签时，推送 `master` 会跳过发布。打开 Actions 里的 **Release** 工作流，选择 `master` 后运行。也可以推送与 `Cargo.toml` 一致的标签：

```bash
git tag v0.1.0
git push origin v0.1.0
```

### 后续版本

1. 提高 `Cargo.toml` 里的 `version`。
2. 运行 `cargo check`，把更新后的 `Cargo.lock` 一起提交。
3. 合并到 `master`。

版本高于最新稳定标签时，流水线创建标签和 GitHub Release，并上传二进制。同一次提交上重新运行会更新这个 Release 的产物。手动运行时可以填写标签；标签必须与 `Cargo.toml` 的版本一致。标签如果已经指向别的提交，流水线会失败并保留原标签。

发布使用 Actions 自带的 `GITHUB_TOKEN`。创建 Release 返回 403 时，把仓库的 Actions 权限改为允许读写 contents。
