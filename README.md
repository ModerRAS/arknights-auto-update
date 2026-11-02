# 明日方舟官服自动更新器

一个 Rust 单二进制程序。它在本机后台自己完成官服更新：启动后约 8 秒检查官方 `clientVersion`，需要换包就下载一次 APK，并拉起已配置的模拟器安装；之后默认每 6 小时再看一次。模拟器没开时，默认每分钟再看一眼；你自己把它打开后，如果官服还落后，就马上安装，这一步不会去拉起模拟器。页面用来看进度和改配置，更新本身不用点按钮。

程序自己也会更新。启动后约 3 秒查看 GitHub Release，默认每 6 小时再看一次。发布构建会对照提交号，所以同一个版本标签指向新提交时也会换程序。需要代理时，在配置里写 `proxy`，官方版本、APK 和程序包都走它。

设备不在线时，用「程序 + 参数数组」拉起 MuMu 12、雷电，或任意自定义模拟器，等 adb 上线后再安装。数据目录里的 `known-versions.json` 记下每台设备上次读到的版本。官方版本没变、而且上次已经装到这个版本时，不会把模拟器再拉起来。

`resVersion` 只展示。它变化表示游戏内资源热更，不更换 APK。

本程序不提供、也不包含任何自动登录或输入账号的功能。遇到 B 服签名冲突时只提示先手动卸载，不会自动卸载。

## 构建与运行

实际最低工具链是 **Rust 1.88**（锁定依赖里的 `time` / ICU 声明了这个版本）。CI 使用当前 stable。原计划里的 1.78 已被这些传递依赖抬高。

```bash
cargo run -- --open
```

默认打开 `http://127.0.0.1:8787`。要完全无人参与，把这个程序放进 Windows 启动文件夹，或用任务计划程序在登录时启动。模拟器条目配好一次即可。常用参数：

| 参数 | 作用 |
| --- | --- |
| `--config <路径>` | 指定 `config.toml`。未指定时，若程序目录已有该文件就用它，否则用系统配置目录 |
| `--bind 127.0.0.1:8787` | 覆盖监听地址，只接受回环地址 |
| `--open` | 启动后用系统浏览器打开页面 |

发布构建：

```bash
cargo build --release
```

产物是 `target/release/ark-updater`（Windows 上为 `ark-updater.exe`）。前端是 `web/` 里的原生 HTML/CSS/JS，由 `rust-embed` 打进二进制，不需要 Node 或 Vite。

日志同时写到控制台、数据目录下按日轮转的 `logs/`，以及页面上的 SSE。

## 配置

示例见 [`config.example.toml`](config.example.toml)，里面有 MuMu 12 和雷电的真实路径示例。程序第一次启动会在配置路径写入一份**不含模拟器条目**的默认配置，避免「全部更新」去执行本机不存在的启动程序。

配置查找顺序：

1. `--config`
2. 程序所在目录的 `config.toml`（便携）
3. Windows：`%APPDATA%\ark-updater\config.toml`；其他系统：`$XDG_CONFIG_HOME/ark-updater/config.toml` 或 `~/.config/ark-updater/config.toml`

WebUI 保存后立即热加载。监听地址的变更要重启进程才生效。

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `listen` | `127.0.0.1:8787` | 只允许 `127.0.0.1` / `localhost` / `::1` |
| `adb_path` | 空 | 空则自动发现。填了但无效时直接报错，不会静默跳过 |
| `apk_cache_dir` | 数据目录下的 `cache` | 下载 `.part` 与完成后的 APK |
| `concurrency` | `3` | 同时安装的设备数，范围 1–16。下载不乘这个数 |
| `version_cache_ttl_secs` | `600` | 官方版本内存缓存 |
| `device_wait_timeout_secs` | `180` | 拉起模拟器后等待 adb 上线 |
| `install_timeout_secs` | `1800` | 单次安装命令超时，至少 60 秒 |
| `auto_launch_game` | `false` | 安装成功后启动官服。启动失败只记警告。默认不替你把游戏打开 |
| `auto_check_interval` | `6h` | `30m` / `6h` / `1d`，或 `off`。开启时，启动后约 8 秒就跑第一轮，而不是先空等一个间隔 |
| `auto_update_on_check` | `true` | 发现更高的 `clientVersion`，或还不知道设备版本时，自动拉起并安装 |
| `proxy` | 空 | 空则使用 `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` 中的第一个。`off` 强制直连。支持 `http`、`https`、`socks5`、`socks5h`。回环地址不走代理 |
| `app_update_interval` | `6h` | 程序自身更新间隔，`off` 关闭。短于 5 分钟时按 5 分钟 |
| `auto_apply_app_update` | `true` | 发现新的程序构建后自动替换并重启。`target/debug` 和 `target/release` 里只提示 |
| `app_update_feed` | 空 | 空则查询 `ModerRAS/arknights-auto-update` 的最新 Release |
| `emulator_watch_interval` | `1m` | 看模拟器开了没有。`off` 关闭。短于 15 秒时按 15 秒。不拉起模拟器 |
| `apk_url_override` | 空 | 非空时跳过官方三层跳转，直接下载该 URL |
| `extra_devices` | `[]` | 手动 `host:port` 或 adb 序列号 |
| `emulators` | `[]` | 见下方 |

adb 自动发现顺序：配置里的 `adb_path` → 环境变量 `ADB` → `PATH` → `%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe` → `C:\platform-tools\adb.exe` → 已探测到的模拟器自带 `adb.exe`。某个模拟器可以单独填 `adb_path`。若 adb server 版本被顶掉，程序会用**当前这次调用的那个 adb** 执行一次 `kill-server` 再 `start-server`，然后重试。

### 模拟器条目

`kind` 为 `mumu` 或 `ldplayer` 时，`start_program` 可以留空，程序会按安装目录或注册表生成默认命令。`custom` 必须自己填程序。

所有外部程序都用 `std::process::Command` 的程序路径加参数数组，不经过 shell，也不把整条命令拼成一个字符串。

- MuMu 12：`mumu-cli.exe control -v <index> launch`。安装依次尝试 `control -v N app install -apk`、`-v N app install -apk`、`api -v N install_apk`，再回退 `adb install -r` 和 `adb install --no-streaming -r`。
- 雷电：`ldconsole.exe launch --index <index>`，安装用 `installapp --index N --filename <apk>`。adb 端口按 `5554 + index * 2`。
- 未填写端口时，MuMu 12 按 `16384 + index * 32` 估计；`mumu-cli info` 的 JSON 里能读到端口时以 JSON 为准。

拉起命令即使返回非 0，只要进程已经启动，仍会在超时内轮询 adb。

## 更新怎么判定

1. `GET https://ak-conf.hypergryph.com/config/prod/official/Android/version`
2. 用 `dumpsys package com.hypergryph.arknights` 取 `versionName`
3. 只按数字分段比较 `clientVersion`（缺段当作 0）。已安装版本不低于官方，且没有勾选强制重装，就停在「已是最新」，不再做 ABI 失败
4. 需要安装时，先看 `ro.product.cpu.abilist` 是否含 `arm64-v8a` 或 `armeabi-v7a`。纯 x86 / x86_64 直接失败，提示需要带 ARM 翻译的模拟器（MuMu 12 / 雷电 9），并且不会开始下载
5. 设备 `/data` 剩余空间已知且小于 4GiB 时失败。读不到 `df` 只告警并继续
6. 解析官方入口 `https://ak.hypergryph.com/downloads/android_lastest`。最终 URL 已是 `.apk`，或 `Content-Type` 是安装包时，直接用作直链，不把约 1.8GB 的响应体读进内存。否则在 HTML 里提取 `.apk` 链接（最多读约 2MB）。官方入口失败时，用备用列表的第一行 `.apk`
7. 多个设备共享同一次下载（singleflight）。`.part` 支持 `Range`；HTTP 200 则从头写；416 则丢掉临时文件重下一次。完成后核对大小和 `PK` 魔数。文件名里的 `arknights-hg-2791` 会和 `2.7.91` 交叉校验；没有这段数字的文件名不因此失败
8. 安装优先走该设备所属模拟器的 CLI，失败再走 adb。同一台设备的 adb 命令串行，设备之间用信号量并发

失败种类：`NoMatchingAbi`、`SignatureConflict`、`InsufficientStorage`、`DownloadFailed`、`InstallFailed`、`DeviceOffline`，以及 `AdbError`、`Cancelled`。

## 程序自身更新

查询 `https://api.github.com/repos/ModerRAS/arknights-auto-update/releases/latest`。只接受 `vMAJOR.MINOR.PATCH`。压缩包名称是 `ark-updater-<标签>-<目标>.tar.gz` 或 `.zip`，旁边有同名 `.sha256`。Linux 用 musl 包，Windows 用 msvc 包。

远端版本更高就更新。版本相同且两边的提交号不同，也更新；这是为了跟上「同一个版本标签会改指向」的发布方式。没有提交号的本地构建，只在远端版本号更高时更新。远端更旧时不降级。

下载后核对 SHA256，只解出 `ark-updater` 或 `ark-updater.exe`，拒绝带 `..` 的路径。新文件先放到程序旁边的 `.new`，再由当前进程拉起一个替换进程：等原进程退出、改名、用原来的参数重启。校验失败不会覆盖正在运行的文件。`cargo` 的 `target` 目录里不会自动替换，页面上的「更新并重启」仍然可以。

官服安装进行时，程序更新会等到它结束再下载和重启。

## 模拟器开机检测

`emulator_watch_interval` 默认 `1m`。每一轮只扫描 adb，不执行模拟器的启动命令。在线设备的官服版本落后，或还不知道版本时，自动安装；同一台设备 10 分钟内不重复开始。设备关掉再打开后，可以马上再试。自动安装关掉时，开机检测只报告状态。

每 6 小时的无人值守仍然会拉起版本落后的离线模拟器。开机检测负责的是「已经开着或刚打开」的设备，不用等到下一轮 6 小时。

## 页面

- 概览：官方 `clientVersion` / `resVersion`、检查时间、APK 缓存、程序版本和更新进度、当前代理、adb 路径和版本
- 开机检测：设备页上单独一行，列出还没开机的模拟器
- 设备表：序列号、型号、Android、ABI、已装版本（未安装会写「未安装」）、状态、独立进度
- 全局：检查更新、扫描设备、全部更新、取消全部。全部更新会处理在线设备，并拉起已配置但还没上线的模拟器，点击前有确认
- 日志：`/api/events` 的 SSE，可按关键字过滤。下载进度推送至少间隔 3 秒
- 设置：adb、缓存目录、并发、自动检查、程序更新、开机检测、代理、模拟器增删改。adb「检测」在路径无效时清空覆盖项并回退到自动发现

HTTP 接口：

```
GET  /api/state
GET  /api/devices
POST /api/devices              { "endpoint": "127.0.0.1:16384" }
POST /api/devices/remove
POST /api/devices/refresh
POST /api/check
POST /api/update               { "devices": [serial] | "all", "force": false }
POST /api/update/cancel
POST /api/app-update/check
POST /api/app-update/apply
GET  /api/config
PUT  /api/config
GET  /api/emulators
POST /api/emulators
PUT  /api/emulators/{id}
DELETE /api/emulators/{id}
POST /api/adb/probe            { "path": "...", "apply_fallback": true }
GET  /api/logs
GET  /api/events
```

## 验收清单

在 Linux 云环境用单元测试、本地假 adb / 本地 HTTP 服务器，以及浏览器操作本机 WebUI 核对。没有 MuMu、雷电或 ARM 安卓设备的步骤无法在本环境跑真机，对应项不打勾。

- [x] 1. `cargo run` 后打开 `http://127.0.0.1:8787` 能看到 WebUI。本环境能访问鹰角时，概览区显示官方 `clientVersion`；网络失败时同一位置显示错误，而不是空白页。
- [x] 2. 设备列表会扫描 `adb devices` 里状态为 `device` 的设备，并用 `dumpsys` 显示 `versionName` 或「未安装」。解析逻辑有单测。本环境没有真实设备时表格为空，这是预期。
- [x] 3. 纯 x86_64 设备在下载前被 ABI 预检拦住，错误文案包含「MuMu 12」，缓存目录不产生下载。见 `task::tests::x86_update_does_not_download`。
- [x] 4. 两台兼容设备同时更新时 APK 只下载一次，安装并发，进度互不影响。见 `task::tests::two_devices_share_one_download`。
- [x] 5. 已有 `.part` 时下次下载带 `Range` 从断点继续。见 `download::tests::resumes_partial_download`。
- [ ] 6. 配置 MuMu 或自定义启动命令后，模拟器未启动时点更新会执行该命令、等到 adb 上线再安装。命令数组、端口公式和「非 0 退出仍继续等待」已在单测覆盖；本环境没有 MuMu / 雷电，未做真机拉起。
- [x] 7. 把 `adb_path` 改成不存在的路径后，状态里会写明配置的 adb 不可用。设置页点「检测」会清空错误路径，并回退到自动发现的 adb（本环境用 PATH 上的假 adb 验证）。
- [x] 8. adb 与模拟器 CLI 都走程序 + 参数数组。含空格和中文的参数在单测里保持独立，不经过 shell。
- [x] 9. `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test` 已通过。版本比较、直链提取、文件名版本、ABI 判定都有单测。Windows 交叉编译由 CI 的 `windows-latest` 作业再编一次。

## 已知限制与假设

- 只监听回环地址。要操作另一台机器上的模拟器，把设备写成可路由的 `host:port`，但 WebUI 本身仍只给本机开。
- 没有 APK 的 SHA256。校验是大小、`PK` 魔数，以及文件名里的版本号（如果文件名带了 `arknights-hg-<数字>`）。
- 宿主磁盘在真正开始写文件前只要求大约 512MB，知道 `Content-Length` 后再要求「文件大小 + 64MB」。设备侧仍然要求 4GiB。读不到宿主剩余空间时只告警。
- `force` 会重装，但有效缓存不会重新下载。
- 下载每写入约 8MB 刷一次盘；取消时会 `sync` 临时文件。
- 安装成功后会再读一次 `versionName`。若读到的版本仍低于官方，判为安装失败。若这次读取本身失败，仍保留安装成功。
- 热重载并发数时，已经在跑的任务继续占用旧的信号量。
- 不扫描整块磁盘找模拟器。Windows 上读注册表和少数固定安装路径；其他系统不探测模拟器目录。
- 用户代理是桌面 Chrome 字符串，避免下载入口返回非浏览器页面。
- 版本号段只取前导数字，所以 `2.7.91-beta` 的最后一段是 91。
- 已经写过的 `config.toml` 不会被改写。若里面仍是 `auto_check_interval = ""` 和 `auto_update_on_check = false`，程序保持手动，需要在设置里改成 `6h` 并勾上自动安装。
- 无人值守依赖进程在跑。登录后不自动启动程序的话，更新也不会发生。
- 离线模拟器是否拉起，看 `known-versions.json` 里上次读到的版本。删掉该文件会让下一轮重新拉起并核对。
- 许可证是仓库既有的 GPL-3.0-only。

## 自动发布

版本号以 `Cargo.toml` 的 `version` 为准，标签是 `v<version>`。推送到 `master` 或打开 Pull Request 时，CI 会在 Ubuntu 和 Windows 上做 `cargo fmt --check`、`cargo clippy -- -D warnings` 和 `cargo test`。Ubuntu 还会跑发布决策脚本。

每次推送到 `master`，Release 工作流都会发布 `v<version>`：还没有这个标签就创建它；版本号升高就新建更高的标签；版本号不变就把现有标签移到这次提交并更新 Release 里的压缩包。`version` 如果低于已有的最高稳定标签，工作流失败，不会改旧标签。也可以在 Actions 里对 `master` 手动运行 **Release**。检查失败时不会上传产物。更新依赖后运行 `cargo update` 再合并，流水线会上传这些压缩包和 SHA256：

| 目标 | 平台 |
| --- | --- |
| `x86_64-unknown-linux-musl` | Linux x86_64，静态链接 |
| `aarch64-unknown-linux-musl` | Linux arm64，静态链接 |
| `x86_64-pc-windows-msvc` | Windows x86_64 |
| `aarch64-apple-darwin` | macOS Apple 芯片 |
| `x86_64-apple-darwin` | macOS Intel |

压缩包内是 `ark-updater`、`LICENSE`、`README.md` 和 `config.example.toml`。同一个版本标签会跟着 `master` 上的最新提交走，所以 `v0.1.0` 这样的标签不是不可变的；要留住旧构建，先把 `Cargo.toml` 的 `version` 提高再合并。更新的推送会取消还在跑的上一次发布。发布使用 Actions 自带的 `GITHUB_TOKEN`，因此工作流自己推上去的标签不会再触发一次发布。
