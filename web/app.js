const state = {
  devices: [],
  logs: [],
  emulators: [],
  settingsDirty: false,
  download: null,
};

const $ = (id) => document.getElementById(id);

function formatBytes(value) {
  if (value == null) return "未知";
  const units = ["B", "KB", "MB", "GB"];
  let number = Number(value);
  let index = 0;
  while (number >= 1024 && index < units.length - 1) {
    number /= 1024;
    index += 1;
  }
  const digits = index === 0 ? 0 : 1;
  return `${number.toFixed(digits)} ${units[index]}`;
}

function formatTime(ms) {
  if (!ms) return "尚未检查";
  return new Date(ms).toLocaleString("zh-CN", { hour12: false });
}

async function request(url, options) {
  const response = await fetch(url, options);
  const data = await response.json().catch(() => ({ ok: false, error: "响应不是 JSON" }));
  if (!response.ok || data.ok === false) {
    throw new Error(data.error || `请求失败（${response.status}）`);
  }
  return data;
}

function toast(message) {
  $("settings-note").textContent = message;
}

async function refreshOverview() {
  const data = await request("/api/state");
  const official = data.official || {};
  $("client-version").textContent = official.client_version || "未知";
  $("res-version").textContent = official.res_version
    ? `resVersion ${official.res_version}（仅展示，不触发 APK 更新）`
    : "resVersion —";
  const cached = official.cached ? " · 缓存" : "";
  $("checked-at").textContent = `${formatTime(official.checked_at_ms)}${cached}`;
  const versionError = $("version-error");
  versionError.hidden = !official.error;
  versionError.textContent = official.error || "";

  state.download = data.download || {};
  const phaseNames = {
    idle: "空闲",
    resolving: "解析直链",
    downloading: "下载中",
    verifying: "校验",
    ready: "已就绪",
    failed: "失败",
  };
  $("apk-phase").textContent = phaseNames[state.download.phase] || state.download.phase || "空闲";
  const percent = Number(state.download.percent || 0);
  $("apk-bar").style.width = `${percent}%`;
  const total = state.download.total ? formatBytes(state.download.total) : "未知大小";
  const path = state.download.path ? ` · ${state.download.path}` : "";
  const err = state.download.error ? ` · ${state.download.error}` : "";
  $("apk-detail").textContent = `${formatBytes(state.download.bytes || 0)} / ${total}${path}${err}`;

  const adb = data.adb || {};
  $("adb-source").textContent = adb.source || "未检测";
  $("adb-path").textContent = adb.path || "未找到 adb";
  $("adb-version").textContent = adb.version || "—";
  const adbError = $("adb-error");
  adbError.hidden = !adb.error;
  adbError.textContent = adb.error || "";
  if (data.restart_required) {
    toast("监听地址已改，需要重启程序后才会生效。");
  }
}

function statusPill(device) {
  const pill = document.createElement("span");
  pill.className = "pill";
  if (!device.online) {
    pill.textContent = device.emulator_id ? "未启动" : "未连接";
  } else if (!device.abi_compatible && device.abi && device.abi !== "-") {
    pill.classList.add("bad");
    pill.textContent = "ABI 不兼容";
  } else if (device.bilibili_installed) {
    pill.textContent = "在线 · 有 B 服";
  } else {
    pill.classList.add("ok");
    pill.textContent = "在线";
  }
  return pill;
}

function progressNode(device) {
  const wrap = document.createElement("div");
  const track = document.createElement("div");
  track.className = "track";
  const bar = document.createElement("div");
  bar.className = "bar";
  const phase = device.task?.phase || "idle";
  if (phase === "installing" || phase === "launching" || phase === "checking" || phase === "waiting_online") {
    bar.classList.add("indet");
  } else {
    bar.style.width = `${Number(device.task?.percent || 0)}%`;
  }
  track.append(bar);
  const msg = document.createElement("span");
  msg.className = "msg";
  msg.textContent = device.task?.message || "";
  wrap.append(track, msg);
  return wrap;
}

function renderDevices() {
  const body = $("device-body");
  body.replaceChildren();
  $("device-empty").hidden = state.devices.length > 0;
  for (const device of state.devices) {
    const row = document.createElement("tr");
    for (const text of [
      device.serial,
      device.emulator_name ? `${device.model} · ${device.emulator_name}` : device.model,
      device.android_version,
      device.abi,
      device.installed_label,
    ]) {
      const cell = document.createElement("td");
      cell.textContent = text || "-";
      row.append(cell);
    }
    const status = document.createElement("td");
    status.append(statusPill(device));
    const progress = document.createElement("td");
    progress.append(progressNode(device));
    const ops = document.createElement("td");
    const actions = document.createElement("div");
    actions.className = "ops";
    const update = document.createElement("button");
    update.type = "button";
    update.textContent = "更新";
    update.addEventListener("click", () => updateDevices([device.serial], $("force-update").checked));
    const filter = document.createElement("button");
    filter.type = "button";
    filter.className = "ghost";
    filter.textContent = "日志";
    filter.addEventListener("click", () => {
      $("log-filter").value = device.serial;
      renderLogs();
    });
    actions.append(update, filter);
    ops.append(actions);
    row.append(status, progress, ops);
    body.append(row);
  }
}

async function loadDevices() {
  const data = await request("/api/devices");
  state.devices = data.devices || [];
  if (state.download && state.download.phase === "downloading") {
    for (const device of state.devices) {
      if (device.task?.phase === "downloading") {
        device.task.percent = state.download.percent || device.task.percent;
      }
    }
  }
  renderDevices();
}

async function updateDevices(devices, force) {
  await request("/api/update", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ devices, force }),
  });
  await loadDevices();
}

function option(value, label, selected) {
  const node = document.createElement("option");
  node.value = value;
  node.textContent = label;
  node.selected = selected;
  return node;
}

function field(label, node) {
  const wrap = document.createElement("label");
  wrap.textContent = label;
  wrap.append(node);
  return wrap;
}

function renderEmulators() {
  const root = $("emulator-list");
  root.replaceChildren();
  state.emulators.forEach((emulator, index) => {
    const card = document.createElement("article");
    card.className = "emu";
    const title = document.createElement("h3");
    title.textContent = emulator.name || "未命名模拟器";
    const grid = document.createElement("div");
    grid.className = "emu-grid";
    const name = document.createElement("input");
    name.value = emulator.name || "";
    name.addEventListener("input", () => {
      emulator.name = name.value;
      state.settingsDirty = true;
    });
    const kind = document.createElement("select");
    kind.append(
      option("mumu", "MuMu", emulator.kind === "mumu"),
      option("ldplayer", "雷电", emulator.kind === "ldplayer"),
      option("custom", "自定义", emulator.kind === "custom"),
    );
    kind.addEventListener("change", () => {
      emulator.kind = kind.value;
      state.settingsDirty = true;
    });
    const indexInput = document.createElement("input");
    indexInput.type = "number";
    indexInput.min = "0";
    indexInput.value = emulator.vmindex ?? 0;
    indexInput.addEventListener("input", () => {
      emulator.vmindex = Number(indexInput.value || 0);
      state.settingsDirty = true;
    });
    const port = document.createElement("input");
    port.value = emulator.adb_port ?? "";
    port.placeholder = "留空则按类型推算";
    port.addEventListener("input", () => {
      emulator.adb_port = port.value.trim() === "" ? null : Number(port.value);
      state.settingsDirty = true;
    });
    const serial = document.createElement("input");
    serial.value = emulator.expected_serial || "";
    serial.placeholder = "127.0.0.1:16384";
    serial.addEventListener("input", () => {
      emulator.expected_serial = serial.value.trim() || null;
      state.settingsDirty = true;
    });
    const program = document.createElement("input");
    program.value = emulator.start_program || "";
    program.placeholder = "留空则自动生成 mumu/ld 命令";
    program.addEventListener("input", () => {
      emulator.start_program = program.value.trim() || null;
      state.settingsDirty = true;
    });
    const args = document.createElement("textarea");
    args.rows = 3;
    args.value = (emulator.start_args || []).join("\n");
    args.addEventListener("input", () => {
      emulator.start_args = args.value.split(/\r?\n/).map((item) => item.trim()).filter(Boolean);
      state.settingsDirty = true;
    });
    const adb = document.createElement("input");
    adb.value = emulator.adb_path || "";
    adb.placeholder = "该模拟器专用 adb，可留空";
    adb.addEventListener("input", () => {
      emulator.adb_path = adb.value.trim() || null;
      state.settingsDirty = true;
    });
    const installDir = document.createElement("input");
    installDir.value = emulator.install_dir || "";
    installDir.addEventListener("input", () => {
      emulator.install_dir = installDir.value.trim() || null;
      state.settingsDirty = true;
    });
    grid.append(
      field("名称", name),
      field("类型", kind),
      field("索引", indexInput),
      field("adb 端口", port),
      field("期望序列号", serial),
      field("专用 adb", adb),
      field("启动程序", program),
      field("安装目录", installDir),
      field("启动参数，每行一个", args),
    );
    const actions = document.createElement("div");
    actions.className = "toolbar";
    const launch = document.createElement("button");
    launch.type = "button";
    launch.textContent = "更新此模拟器";
    launch.addEventListener("click", () => updateDevices([emulator.id || emulator.expected_serial], $("force-update").checked));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "ghost";
    remove.textContent = "删除";
    remove.addEventListener("click", () => {
      state.emulators.splice(index, 1);
      state.settingsDirty = true;
      renderEmulators();
    });
    actions.append(launch, remove);
    card.append(title, grid, actions);
    root.append(card);
  });
}

function fillSettings(config) {
  const form = $("settings-form");
  form.listen.value = config.listen || "";
  form.concurrency.value = config.concurrency ?? 3;
  form.version_cache_ttl_secs.value = config.version_cache_ttl_secs ?? 600;
  form.device_wait_timeout_secs.value = config.device_wait_timeout_secs ?? 180;
  form.install_timeout_secs.value = config.install_timeout_secs ?? 1800;
  form.auto_check_interval.value = config.auto_check_interval || "";
  form.adb_path.value = config.adb_path || "";
  form.apk_cache_dir.value = config.apk_cache_dir || "";
  form.apk_url_override.value = config.apk_url_override || "";
  form.extra_devices.value = (config.extra_devices || []).join("\n");
  form.auto_launch_game.checked = Boolean(config.auto_launch_game);
  form.auto_update_on_check.checked = Boolean(config.auto_update_on_check);
  state.emulators = (config.emulators || []).map((item) => ({ ...item, start_args: item.start_args || [] }));
  renderEmulators();
  $("settings-note").textContent = `配置文件：${config.pathHint || ""}`;
}

async function loadSettings(force) {
  if (state.settingsDirty && !force) return;
  const data = await request("/api/config");
  data.config.pathHint = data.config_path;
  fillSettings(data.config);
  $("settings-note").textContent = `配置文件：${data.config_path}`;
  state.settingsDirty = false;
}

function blank(value) {
  const text = (value || "").trim();
  return text ? text : null;
}

function readSettings() {
  const form = $("settings-form");
  return {
    listen: form.listen.value.trim(),
    concurrency: Number(form.concurrency.value || 3),
    version_cache_ttl_secs: Number(form.version_cache_ttl_secs.value || 600),
    device_wait_timeout_secs: Number(form.device_wait_timeout_secs.value || 180),
    install_timeout_secs: Number(form.install_timeout_secs.value || 1800),
    auto_check_interval: form.auto_check_interval.value.trim(),
    auto_launch_game: form.auto_launch_game.checked,
    auto_update_on_check: form.auto_update_on_check.checked,
    adb_path: blank(form.adb_path.value),
    apk_cache_dir: blank(form.apk_cache_dir.value),
    apk_url_override: blank(form.apk_url_override.value),
    extra_devices: form.extra_devices.value.split(/\r?\n/).map((item) => item.trim()).filter(Boolean),
    emulators: state.emulators.map((item) => ({
      ...item,
      vmindex: Number(item.vmindex || 0),
      adb_port: item.adb_port === "" || item.adb_port == null ? null : Number(item.adb_port),
      start_args: item.start_args || [],
    })),
  };
}

function renderLogs() {
  const filter = $("log-filter").value.trim().toLowerCase();
  const lines = state.logs.filter((line) => {
    if (!filter) return true;
    return `${line.level} ${line.target} ${line.message}`.toLowerCase().includes(filter);
  });
  $("log-view").textContent = lines
    .map((line) => {
      const time = new Date(line.ts_ms).toLocaleTimeString("zh-CN", { hour12: false });
      return `${time} ${line.level} ${line.message}`;
    })
    .join("\n");
  const view = $("log-view");
  view.scrollTop = view.scrollHeight;
}

function pushLog(line) {
  state.logs.push(line);
  if (state.logs.length > 400) state.logs.shift();
  renderLogs();
}

function connectEvents() {
  const source = new EventSource("/api/events");
  source.addEventListener("log", (event) => {
    const data = JSON.parse(event.data);
    pushLog(data);
  });
  source.addEventListener("download", () => {
    refreshOverview().catch(() => {});
    loadDevices().catch(() => {});
  });
  source.addEventListener("device", () => {
    loadDevices().catch(() => {});
  });
  source.onerror = () => {
    source.close();
    setTimeout(connectEvents, 2000);
  };
}

document.querySelectorAll(".tab").forEach((button) => {
  button.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((item) => {
      item.classList.toggle("active", item === button);
      item.setAttribute("aria-selected", item === button ? "true" : "false");
    });
    const showDevices = button.dataset.tab === "devices";
    $("view-devices").hidden = !showDevices;
    $("view-settings").hidden = showDevices;
    if (!showDevices) loadSettings(false).catch((error) => toast(error.message));
  });
});

$("btn-check").addEventListener("click", () => {
  request("/api/check", { method: "POST" }).then(refreshOverview).catch((error) => {
    $("version-error").hidden = false;
    $("version-error").textContent = error.message;
  });
});
$("btn-refresh").addEventListener("click", () => {
  request("/api/devices/refresh", { method: "POST" })
    .then((data) => {
      state.devices = data.devices || [];
      renderDevices();
      return refreshOverview();
    })
    .catch((error) => {
      $("adb-error").hidden = false;
      $("adb-error").textContent = error.message;
    });
});
$("btn-update-all").addEventListener("click", () => {
  const ok = window.confirm("将更新所有在线设备，并拉起未启动的已配置模拟器。APK 只下载一次。继续？");
  if (!ok) return;
  updateDevices("all", $("force-update").checked).catch((error) => alert(error.message));
});
$("btn-cancel").addEventListener("click", () => {
  request("/api/update/cancel", { method: "POST" }).then(loadDevices).catch((error) => alert(error.message));
});
$("quick-add").addEventListener("submit", (event) => {
  event.preventDefault();
  const endpoint = $("quick-endpoint").value.trim();
  request("/api/devices", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ endpoint }),
  }).then((data) => {
    state.devices = data.devices || [];
    renderDevices();
    $("quick-endpoint").value = "";
  }).catch((error) => alert(error.message));
});
$("settings-form").addEventListener("input", () => {
  state.settingsDirty = true;
});
$("settings-form").addEventListener("submit", (event) => {
  event.preventDefault();
  request("/api/config", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(readSettings()),
  }).then((data) => {
    state.settingsDirty = false;
    if (data.adb_error) {
      toast(`已保存，但 adb 不可用：${data.adb_error}`);
    } else if (data.restart_required) {
      toast("已保存。监听地址变更需要重启程序。");
    } else {
      toast("已保存，并已热加载。");
    }
    return refreshOverview();
  }).catch((error) => toast(error.message));
});
$("btn-reload-config").addEventListener("click", () => {
  state.settingsDirty = false;
  loadSettings(true).catch((error) => toast(error.message));
});
$("btn-probe").addEventListener("click", async () => {
  const path = $("settings-form").adb_path.value.trim();
  try {
    const response = await fetch("/api/adb/probe", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path, apply_fallback: true }),
    });
    const data = await response.json();
    if (data.ok) {
      toast(`adb 可用：${data.path}（${data.version}）`);
    } else if (data.fallback_applied) {
      $("settings-form").adb_path.value = "";
      state.settingsDirty = false;
      toast(`路径无效：${data.error}。已回退到自动发现的 ${data.path}（${data.version || "未知版本"}）`);
    } else {
      toast(data.error || "检测失败，也没有可用的自动发现结果");
    }
    await refreshOverview();
  } catch (error) {
    toast(error.message);
  }
});
$("btn-add-emu").addEventListener("click", () => {
  state.emulators.push({
    id: "",
    name: "新模拟器",
    kind: "mumu",
    vmindex: state.emulators.length,
    start_program: null,
    start_args: [],
    expected_serial: null,
    adb_port: null,
    adb_path: null,
    install_dir: null,
  });
  state.settingsDirty = true;
  renderEmulators();
});
$("log-filter").addEventListener("input", renderLogs);

request("/api/logs").then((data) => {
  state.logs = data.logs || [];
  renderLogs();
}).catch(() => {});
refreshOverview().catch((error) => {
  $("version-error").hidden = false;
  $("version-error").textContent = error.message;
});
loadDevices().catch(() => {});
connectEvents();
setInterval(() => {
  refreshOverview().catch(() => {});
  loadDevices().catch(() => {});
}, 2000);
