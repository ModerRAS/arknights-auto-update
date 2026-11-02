# 需求文档

## 介绍

`ark-updater` 需要持续集成，以及在版本前进时自动构建并发布到 GitHub Releases。版本以 `Cargo.toml` 的 `version` 为准，发布标签为 `v<version>`。发布物是各平台可执行文件压缩包和 SHA256 校验，供没有 Rust 工具链的环境下载运行。

## 需求

### 需求 1 - 持续集成

**用户故事：** 作为维护者，我希望每次合并前都自动检查格式、静态分析和测试，以便损坏的提交不能进入 `master`。

#### 验收标准

1. When 打开 Pull Request 或向 `master` 推送提交时，持续集成应当运行 `cargo fmt --all -- --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked --all-targets`、发布决策脚本测试，以及 `cargo build --locked --release`。
2. When 上述任一检查失败时，持续集成应当使该工作流失败。
3. While 多个检查针对同一引用同时运行, when 新的提交到达时，持续集成应当取消该引用上仍在进行的旧检查。

### 需求 2 - 每次推送 master 自动发布

**用户故事：** 作为维护者，我希望每次把提交推送到 `master` 都自动得到 GitHub Release，以便不用手工上传二进制。

#### 验收标准

1. When 提交被推送到 `master`，且 `Cargo.toml` 的 `version` 不低于最新稳定标签（仓库还没有任何稳定标签时也算满足）时，发布流水线应当发布标签 `v<version>` 和对应的 GitHub Release，并上传发布产物。
2. While 标签 `v<version>` 已经指向其他提交, when 版本号不变的新提交被推送到 `master` 时，发布流水线应当把该标签移动到当前提交，并更新对应 Release 的产物。
3. While 正在自动发布, when 格式检查、Clippy、测试或发布决策脚本测试失败时，发布流水线应当失败并且不上传产物。
4. While `Cargo.toml` 的 `version` 低于最新稳定标签, when 提交被推送到 `master` 时，发布流水线应当失败，并且不移动已有标签。
5. When 发布流水线创建或移动标签时，该标签应当指向触发这次运行的提交。
6. While 稳定标签 `v<version>` 已指向当前提交, when 发布流水线再次在该提交上运行时，发布流水线应当更新该 Release 的产物，并且不改变标签指向。
7. While 同一次仓库已经有发布在进行, when 新的提交被推送到 `master` 时，发布流水线应当取消仍在进行的旧发布。

### 需求 3 - 手动发布

**用户故事：** 作为维护者，我希望可以在 `master` 上再手动发布一次当前版本。

#### 验收标准

1. When 在 `master` 上手动运行 Release 工作流且未填写标签时，发布流水线应当按 `Cargo.toml` 的 `version` 发布 `v<version>`。
2. When 手动运行 Release 工作流时填写了标签, the 发布流水线 shall 仅在该标签为 `v<version>` 且与 `Cargo.toml` 版本一致时发布。
3. When 从 `master` 以外的分支手动运行 Release 工作流时，发布流水线应当失败。
4. While 目标标签已指向其他提交, when 在 `master` 上手动发布当前版本时，发布流水线应当把该标签移动到当前提交。
5. While `Cargo.toml` 的 `version` 低于最新稳定标签, when 手动运行 Release 工作流时，发布流水线应当失败，并且不移动已有标签。

### 需求 4 - 版本与标签一致

**用户故事：** 作为使用者，我希望下载到的程序版本和 Release 标签一致，以便校验升级结果。

#### 验收标准

1. When 推送的标签不是不含前导零的 `vMAJOR.MINOR.PATCH`，或与 `Cargo.toml` 的 `version` 不一致时，发布流水线应当失败并且不上传产物。
2. When 推送的稳定标签与 `Cargo.toml` 版本一致，但该标签引用的提交不是这次工作流的提交时，发布流水线应当失败，并且不移动该标签。
3. While `Cargo.toml` 的 `version` 低于最新稳定标签, when 推送与该版本一致的标签时，发布流水线应当失败，并且不移动已有标签。
4. When `Cargo.toml` 的 `version` 不是不含前导零的 `MAJOR.MINOR.PATCH` 时，测试应当失败。

### 需求 5 - 发布产物

**用户故事：** 作为使用者，我希望从 GitHub Release 下载对应平台的压缩包，以便直接运行程序。

#### 验收标准

1. When 发布成功时，发布流水线应当上传以下平台的压缩包：Linux x86_64 musl、Linux aarch64 musl、Windows x86_64 MSVC、macOS aarch64、macOS x86_64。
2. When 发布成功时，每个压缩包应当包含可执行文件、`LICENSE` 和 `README.md`，并附带 SHA256 校验文件。
3. When 发布成功时，GitHub Release 应当标记为最新版本，并且不是草稿或预发布。

### 需求 6 - 命令行版本入口

**用户故事：** 作为使用者，我希望运行已发布的程序就能看到它的版本，以便确认安装的是哪一次发布。

#### 验收标准

1. When 执行 `ark-updater --version` 或 `-V` 时，程序应当以退出码 0 输出版本号，且该版本号与 `Cargo.toml` 一致。
2. When 执行 `ark-updater --help` 或 `-h` 时，程序应当以退出码 0 输出用法，并且不启动 WebUI。
3. When 参数无法识别时，程序应当以退出码 2 退出。
