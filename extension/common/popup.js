const ALL_SITES = { origins: ["<all_urls>"] };

const elements = {
  status: document.getElementById("status"),
  notice: document.getElementById("notice"),
  capture: document.getElementById("capture"),
  linkForm: document.getElementById("link-form"),
  link: document.getElementById("link"),
  media: document.getElementById("media"),
  mediaEmpty: document.getElementById("media-empty"),
  mediaCount: document.getElementById("media-count"),
  mediaTemplate: document.getElementById("media-item"),
  openApp: document.getElementById("open-app"),
  options: document.getElementById("options"),
  permissionCard: document.getElementById("permission-card"),
  grant: document.getElementById("grant"),
};

function errorText(error) {
  return String(error?.message ?? error);
}

/** Asks the background script; a failure to reach it comes back as an error reply. */
async function ask(message) {
  try {
    return await browser.runtime.sendMessage(message);
  } catch (error) {
    return { type: "error", message: errorText(error) };
  }
}

function showNotice(text, kind = "info") {
  elements.notice.textContent = text;
  elements.notice.dataset.kind = kind;
  elements.notice.hidden = false;
}

/** A `.catch` handler that tells the user what failed. */
function reportFailure(what) {
  return (error) => showNotice(`${what}: ${errorText(error)}`, "error");
}

function setStatus(state, label) {
  elements.status.dataset.state = state;
  elements.status.textContent = label;
}

async function refreshStatus() {
  const reply = await ask({ type: "status" });
  if (reply?.type === "status" && reply.app_running) {
    setStatus("connected", "Connected");
  } else if (reply?.type === "status") {
    setStatus("idle", "App closed");
    elements.status.title = "Downpour starts automatically when you send it a download.";
  } else {
    setStatus("error", "Not set up");
    showNotice(reply?.message ?? "The native host did not answer.", "error");
  }
}

/** Reports the outcome of handing downloads to the app. */
function reportDelivery(reply) {
  if (reply?.type === "accepted") {
    const count = reply.count ?? 1;
    showNotice(count === 1 ? "Sent to Downpour." : `Sent ${count} downloads to Downpour.`, "success");
    refreshStatusReported();
  } else {
    showNotice(reply?.message ?? "Downpour did not answer.", "error");
  }
}

function refreshStatusReported() {
  refreshStatus().catch((error) => {
    setStatus("error", "Not set up");
    reportFailure("Could not check Downpour")(error);
  });
}

async function activeTab() {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  return tab;
}

/** Badge, name, and detail line for a detected file or stream. */
function describeMedia(item) {
  if (item.kind === "stream") {
    const format = item.format === "dash" ? "DASH" : "HLS";
    return {
      badge: format,
      name: item.title || DownpourRules.fileNameFromUrl(item.url) || "Video stream",
      meta: item.heights?.length
        ? `${format} video · ${DownpourRules.qualitySummary(item.heights)}`
        : `${format} video stream`,
    };
  }
  const extension = DownpourRules.fileExtension(item.fileName) || item.mime?.split("/")[1] || "media";
  return {
    badge: extension.slice(0, 4).toUpperCase(),
    name: item.fileName || item.url,
    meta: `${item.mime} · ${DownpourRules.formatBytes(item.size)}`,
  };
}

async function renderMedia() {
  const tab = await activeTab();
  if (!tab) {
    return;
  }
  const reply = await ask({ type: "tab-media", tabId: tab.id });
  if (reply?.type !== "media") {
    showNotice(reply?.message ?? "Could not list this page's media.", "error");
    return;
  }
  const items = reply.items;
  elements.media.replaceChildren();
  elements.mediaEmpty.hidden = items.length > 0;
  elements.mediaCount.textContent = `${items.length} found`;
  for (const item of items) {
    const node = elements.mediaTemplate.content.firstElementChild.cloneNode(true);
    const view = describeMedia(item);
    node.querySelector(".media-badge").textContent = view.badge;
    node.querySelector(".media-badge").dataset.kind = item.kind ?? "file";
    node.querySelector(".media-name").textContent = view.name;
    node.querySelector(".media-name").title = item.url;
    node.querySelector(".media-meta").textContent = view.meta;
    const button = node.querySelector(".media-download");
    button.addEventListener("click", async () => {
      button.disabled = true;
      const reply = await ask({ type: "download-media", tabId: tab.id, url: item.url });
      reportDelivery(reply);
      // A refused download can be tried again.
      button.disabled = reply?.type === "accepted";
    });
    elements.media.append(node);
  }
}

async function initCaptureToggle() {
  const { captureEnabled } = await browser.storage.local.get({ captureEnabled: DownpourRules.DEFAULT_SETTINGS.captureEnabled });
  elements.capture.checked = captureEnabled;
  elements.capture.addEventListener("change", () => {
    const captureEnabled = elements.capture.checked;
    browser.storage.local.set({ captureEnabled }).catch((error) => {
      elements.capture.checked = !captureEnabled;
      reportFailure("Could not save the capture setting")(error);
    });
  });
}

/** Firefox grants MV3 host access separately from installation. */
async function initPermissions() {
  if (await browser.permissions.contains(ALL_SITES)) {
    return;
  }
  elements.permissionCard.hidden = false;
  elements.grant.addEventListener("click", async () => {
    try {
      if (await browser.permissions.request(ALL_SITES)) {
        elements.permissionCard.hidden = true;
      }
    } catch (error) {
      reportFailure("Could not ask for site access")(error);
    }
  });
}

elements.linkForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const url = elements.link.value.trim();
  // The background reads the tab's address and container from its id.
  const tab = await activeTab().catch(() => undefined);
  const reply = await ask({ type: "download-url", url, tabId: tab?.id });
  reportDelivery(reply);
  if (reply?.type === "accepted") {
    elements.link.value = "";
  }
});

elements.openApp.addEventListener("click", async () => {
  const reply = await ask({ type: "show" });
  if (reply?.type === "error") {
    showNotice(reply.message, "error");
  } else {
    window.close();
  }
});

elements.options.addEventListener("click", (event) => {
  event.preventDefault();
  browser.runtime.openOptionsPage().catch(reportFailure("Could not open the options"));
});

initCaptureToggle().catch(reportFailure("Could not read the capture setting"));
initPermissions().catch(reportFailure("Could not check site access"));
refreshStatusReported();
renderMedia().catch(reportFailure("Could not list this page's media"));
