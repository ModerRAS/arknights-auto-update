# 需求文档

## 介绍

明日方舟官服自动更新器是一个 Windows 桌面工具，产物为单个 Rust 可执行文件。它在本机提供 WebUI，展示已连接安卓设备上的官服版本，并在需要更换 APK 时下载一次安装包、并发安装到多台设备。设备离线时，可按配置拉起模拟器并等待 adb 上线。资源热更号只展示，不触发 APK 下载。程序不提供账号登录。

## 需求

### 需求 1 - 官方版本展示

**用户故事：** 作为玩家，我希望打开本机页面就能看到官服最新客户端版本，以便判断要不要换 APK。

#### 验收标准

1. When 程序启动并完成官方版本查询，the 明日方舟更新器 shall 在 WebUI 概览区显示 `clientVersion` 与 `resVersion`。
2. When `resVersion` 变化而 `clientVersion` 不变，the 明日方舟更新器 shall 不因此下载或安装 APK。
3. While 内存缓存未过期，when 用户再次检查版本，the 明日方舟更新器 shall 复用缓存结果，并标明该结果来自缓存。
4. When 版本接口失败，the 明日方舟更新器 shall 在概览区显示错误，并保留上一次成功的版本（如果有）。

### 需求 2 - 设备识别

**用户故事：** 作为玩家，我希望看到每台模拟器或真机的型号、系统、ABI 和已装方舟版本。

#### 验收标准

1. When 用户扫描设备，the 明日方舟更新器 shall 只列出 `adb devices` 中状态为 `device` 的序列号，并尝试连接常见模拟器端口与手动添加的地址。
2. When 设备已安装 `com.hypergryph.arknights`，the 明日方舟更新器 shall 显示 `versionName`。
3. When 该包未安装，the 明日方舟更新器 shall 显示「未安装」。
4. When 用户添加 `host:port`，the 明日方舟更新器 shall 保存该地址并尝试 `adb connect`。

### 需求 3 - 更新前预检

**用户故事：** 作为玩家，我不希望在纯 x86 模拟器上白等 1.8GB 下载。

#### 验收标准

1. While 已装版本不低于官方 `clientVersion` 且未要求强制重装，when 用户更新该设备，the 明日方舟更新器 shall 记为已是最新，并且不把 ABI 不兼容报成失败。
2. While 需要安装且 `ro.product.cpu.abilist` 不含 `arm64-v8a` 与 `armeabi-v7a`，when 用户更新，the 明日方舟更新器 shall 在任何下载开始前失败，并提示需要带 ARM 翻译的模拟器（MuMu 12 / 雷电 9）。
3. While 设备可用空间已知且小于 4GiB，when 用户更新，the 明日方舟更新器 shall 以存储空间不足失败。
4. When 安装输出包含 `INSTALL_FAILED_UPDATE_INCOMPATIBLE`，the 明日方舟更新器 shall 提示签名冲突并要求用户手动卸载，且不得自动卸载。

### 需求 4 - 一次下载与并发安装

**用户故事：** 作为同时开多台模拟器的玩家，我希望安装包只下载一次，安装可以并行。

#### 验收标准

1. While 两台或更多设备同时需要同一 `clientVersion`，when 更新开始，the 明日方舟更新器 shall 只发起一次 APK 下载，并让这些设备共享产物。
2. When 下载中断后再次更新，the 明日方舟更新器 shall 用 `.part` 与 HTTP Range 从断点继续。
3. When 多个设备安装，the 明日方舟更新器 shall 用可配置并发上限限制同时安装数，默认 3，且同一设备上的 adb 命令串行。
4. When 安装进行中，the 明日方舟更新器 shall 为每台设备单独推送进度；下载进度推送间隔不得短于 3 秒。
5. When 用户取消，the 明日方舟更新器 shall 停止尚未完成的下载与安装，并保留已写入的 `.part`。

### 需求 5 - 拉起模拟器

**用户故事：** 作为玩家，我希望模拟器没开时点一次更新就能把它拉起来再装包。

#### 验收标准

1. While 目标设备不在线且已配置启动命令，when 用户更新该模拟器，the 明日方舟更新器 shall 以程序加参数数组启动外部命令，不得拼接 shell 字符串。
2. While `kind` 为 `mumu` 或 `ldplayer` 且启动程序为空，when 用户更新，the 明日方舟更新器 shall 生成该模拟器的默认启动命令。
3. While `kind` 为 `custom` 且未填启动程序，when 用户保存配置，the 明日方舟更新器 shall 拒绝该配置。
4. When 启动命令已执行，the 明日方舟更新器 shall 在可配置超时（默认 180 秒）内轮询 adb，上线后继续更新。

### 需求 6 - adb 路径

**用户故事：** 作为玩家，我希望知道当前用的是哪一个 adb，并在填错时能改回来。

#### 验收标准

1. When 配置显式指定了 `adb_path` 且该文件不可用，the 明日方舟更新器 shall 报告该路径错误，不得静默改用下一个候选。
2. While 未指定 `adb_path`，when 程序发现 adb，the 明日方舟更新器 shall 按环境变量、PATH、Android SDK、固定 platform-tools 路径、模拟器自带 adb 的顺序采用第一个可用项。
3. When 用户在 WebUI 对一个错误路径点击「检测」，the 明日方舟更新器 shall 清空该覆盖路径，并显示自动发现的 adb 及其版本。
4. When 某个模拟器配置了自己的 adb，the 明日方舟更新器 shall 用该二进制连接和操作对应实例。

### 需求 7 - 本机 WebUI 与配置

**用户故事：** 作为玩家，我希望用浏览器完成本地配置，并且改完就生效。

#### 验收标准

1. When 程序监听，the 明日方舟更新器 shall 只绑定回环地址，默认 `127.0.0.1:8787`，并托管内嵌静态页面。
2. When 用户保存设置或增删模拟器，the 明日方舟更新器 shall 把配置写入 `config.toml` 并热加载。
3. When 用户打开日志，the 明日方舟更新器 shall 通过 SSE 推送日志与进度，并允许按关键字过滤。
4. While `auto_check_interval` 为空或 `off`，when 程序运行，the 明日方舟更新器 shall 不自动检查。默认即为关闭。
5. The 明日方舟更新器 shall 不提供自动登录或输入账号的界面与接口。
