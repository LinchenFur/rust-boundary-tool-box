const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => Array.from(document.querySelectorAll(selector));

const modalLayer = $("#modalLayer");
const modalTitle = $("#modalTitle");
const modalText = $("#modalText");
const progressWrap = $("#progressWrap");
const progressBar = $("#progressBar");
const progressDetail = $("#progressDetail");
const toast = $("#toast");

function wireLiquidMotion(root = document) {
  root.querySelectorAll("button.lggc").forEach((element) => {
    if (element.dataset.liquidMotion === "ready") {
      return;
    }
    element.dataset.liquidMotion = "ready";

    element.addEventListener("pointermove", (event) => {
      const rect = element.getBoundingClientRect();
      const x = ((event.clientX - rect.left) / Math.max(rect.width, 1)) * 100;
      const y = ((event.clientY - rect.top) / Math.max(rect.height, 1)) * 100;
      element.style.setProperty("--mx", `${x}%`);
      element.style.setProperty("--my", `${y}%`);
      element.classList.add("liquid-hover");
    });

    element.addEventListener("pointerleave", () => {
      element.classList.remove("liquid-hover", "is-pressing");
      element.style.setProperty("--mx", "50%");
      element.style.setProperty("--my", "50%");
    });

    element.addEventListener("pointerdown", (event) => {
      if (element.disabled) {
        return;
      }
      element.classList.add("is-pressing");
      spawnRipple(element, event);
    });

    element.addEventListener("pointerup", () => {
      element.classList.remove("is-pressing");
    });
  });
}

function spawnRipple(element, event) {
  const rect = element.getBoundingClientRect();
  const ripple = document.createElement("span");
  ripple.className = "liquid-ripple";
  ripple.style.setProperty("--ripple-x", `${event.clientX - rect.left}px`);
  ripple.style.setProperty("--ripple-y", `${event.clientY - rect.top}px`);
  element.appendChild(ripple);
  ripple.addEventListener("animationend", () => ripple.remove(), { once: true });
}

function send(command, payload = {}) {
  if (window.chrome && window.chrome.webview) {
    window.chrome.webview.postMessage({ command, payload });
  }
}

function showModal(title, message, options = {}) {
  modalTitle.textContent = title || "提示";
  modalText.textContent = message || "";
  progressWrap.classList.toggle("hidden", !options.progress);
  modalLayer.classList.remove("hidden");
}

function closeModal() {
  modalLayer.classList.add("hidden");
  progressWrap.classList.add("hidden");
}

function showToast(message) {
  toast.textContent = message;
  toast.classList.remove("hidden");
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => toast.classList.add("hidden"), 2600);
}

function appendLog(line) {
  const output = $("#logOutput");
  const text = output.textContent === "等待操作..." ? "" : output.textContent + "\n";
  output.textContent = `${text}${line}`.trim();
  output.scrollTop = output.scrollHeight;
}

function setBusy(busy) {
  $$("button").forEach((button) => {
    if (button.dataset.command === "cancel-install" || button.dataset.windowCommand) {
      return;
    }
    if (busy) {
      button.dataset.wasDisabled = button.disabled ? "true" : "false";
      button.disabled = true;
    } else if (button.dataset.wasDisabled === "false") {
      button.disabled = false;
    }
  });
}

function updateTarget(path, source) {
  $("#targetPath").textContent = path;
  $("#directoryTarget").textContent = path;
  $("#targetInput").value = path;
  $("#directoryInput").value = path;
  if (source) {
    $("#targetSource").textContent = source;
  }
}

function updatePorts(rows) {
  const list = $("#portList");
  list.textContent = "";
  if (!rows.length) {
    list.innerHTML = '<div class="empty-state">暂无端口数据</div>';
    return;
  }
  rows.forEach((row) => {
    const item = document.createElement("article");
    item.className = `port-row ${row.status === "占用" ? "busy" : ""}`;
    item.innerHTML = `
      <b>${row.protocol}/${row.port}</b>
      <span>${row.status === "占用" ? `${row.name || "未知进程"} PID ${row.pid}` : "空闲"}</span>
      <em>${row.expected ? "目标进程" : row.status}</em>
    `;
    list.appendChild(item);
  });
  wireLiquidMotion(list);
}

window.boundaryNative = {
  onHostEvent(event) {
    switch (event.type) {
      case "state":
        $("#appVersion").textContent = event.appVersion || "";
        $("#payloadState").textContent = event.payload || "";
        $("#proxyState").textContent = event.proxy || "";
        $("#proxyInput").value = event.proxy || "";
        $("#adminState").textContent = event.isAdmin ? "管理员模式" : "未使用管理员模式启动";
        $("#adminBanner").classList.toggle("hidden", Boolean(event.isAdmin));
        $$('[data-command="install"], [data-command="uninstall"]').forEach((button) => {
          button.disabled = !event.isAdmin;
        });
        break;
      case "status":
        $("#statusText").textContent = event.text || "就绪";
        $("#statusDetail").textContent = event.detail || "";
        break;
      case "target":
        updateTarget(event.path || "未锁定", event.source || "");
        break;
      case "ports":
        updatePorts(event.rows || []);
        break;
      case "busy":
        $("#statusText").textContent = event.busy ? `${event.title}中` : "就绪";
        setBusy(Boolean(event.busy));
        break;
      case "progress":
        if (event.open === false) {
          closeModal();
          break;
        }
        showModal(event.title, event.detail, { progress: true });
        progressBar.style.width = `${Math.max(0, Math.min(1, event.value || 0)) * 100}%`;
        progressDetail.textContent = event.detail || "";
        break;
      case "action-result":
        if (event.progressClosed) {
          progressWrap.classList.add("hidden");
        }
        showModal(event.title, event.message || "");
        appendLog(`${event.title}: ${event.message || ""}`);
        break;
      case "dialog":
        showModal(event.title, event.message || "");
        break;
      case "toast":
        showToast(event.message || "");
        break;
      case "log":
        appendLog(event.line || "");
        break;
      default:
        appendLog(JSON.stringify(event));
    }
  },
};

$$("[data-page]").forEach((tab) => {
  tab.addEventListener("click", () => {
    $$("[data-page]").forEach((item) => item.classList.remove("active"));
    $$(".view").forEach((page) => page.classList.remove("active"));
    tab.classList.add("active");
    $(`#page-${tab.dataset.page}`).classList.add("active");
  });
});

$$("button[data-command]").forEach((button) => {
  button.addEventListener("click", () => {
    const payload = {};
    if (button.dataset.fromInput) {
      payload.path = $(`#${button.dataset.fromInput}`)?.value || "";
      payload.value = payload.path;
    }
    if (button.dataset.proxyDefault) {
      payload.value = "https://git-proxy.cubland.icu/";
    }
    send(button.dataset.command, payload);
  });
});

$$("[data-window-command]").forEach((button) => {
  button.addEventListener("click", () => send(button.dataset.windowCommand));
});

$$(".flag").forEach((button) => {
  button.addEventListener("click", () => {
    $$(".flag").forEach((item) => item.classList.remove("active"));
    button.classList.add("active");
    showToast("语言资源会随 WebView UI 迁移继续接入。");
  });
});

$("#modalClose").addEventListener("click", closeModal);
modalLayer.addEventListener("click", (event) => {
  if (event.target === modalLayer && progressWrap.classList.contains("hidden")) {
    closeModal();
  }
});

window.addEventListener("DOMContentLoaded", () => {
  wireLiquidMotion();
  send("ready");
  send("refresh-ports");
});
