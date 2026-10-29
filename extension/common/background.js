// Background script of the Firefox add-on (MV3 event page). The manifest
// loads capture-rules.js before it.
//
// Talks to the Downpour app through the native messaging host `downpour.native_host`:
// one `sendNativeMessage` per request, so no port has to stay open while the
// event page sleeps.

const HOST = "downpour.native_host";
const MENU = { link: "downpour-link", media: "downpour-media", page: "downpour-page" };
const MAX_MEDIA_PER_TAB = 25;
// Downloads we hand back to the browser after the app could not be reached;
// onCreated must not capture them again.
const BYPASS_TTL_MS = 60_000;
const bypassed = DownpourRules.createBypassList(BYPASS_TTL_MS, Date.now);

// ---- Native host ----

async function sendToHost(message) {
  try {
    const reply = await browser.runtime.sendNativeMessage(HOST, message);
    if (!reply) {
      return { type: "error", message: "The native host closed without replying." };
    }
    return reply;
  } catch (error) {
    return { type: "error", message: hostErrorMessage(error) };
  }
}

function hostErrorMessage(error) {
  const text = String(error?.message ?? error);
  if (/not found|no such native application|not registered/i.test(text)) {
    return "Downpour's browser integration is not registered. Open Downpour → Settings → Register browser integration.";
  }
  return text;
}

async function sendDownloads(items) {
  return sendToHost({ type: "download", items });
}

// ---- Settings ----

async function loadSettings() {
  const stored = await browser.storage.local.get(DownpourRules.DEFAULT_SETTINGS);
  return { ...DownpourRules.DEFAULT_SETTINGS, ...stored };
}

// ---- Building requests ----

/** `storeId` is the private window's or container's store; absent means the default one. */
async function cookiesFor(url, storeId) {
  try {
    const cookies = await browser.cookies.getAll(storeId ? { url, storeId } : { url });
    return DownpourRules.cookieHeader(cookies) || undefined;
  } catch {
    // Firefox with first-party isolation requires a firstPartyDomain; the
    // download can still work without cookies.
    return undefined;
  }
}

async function buildItem(url, details = {}) {
  return {
    url,
    fileName: details.fileName || undefined,
    referrer: details.referrer || undefined,
    cookies: await cookiesFor(url, details.cookieStoreId),
    userAgent: navigator.userAgent,
    mime: details.mime || undefined,
    // 0 is what the browser reports while it does not know the size.
    size: Number.isFinite(details.size) && details.size > 0 ? details.size : undefined,
    stream: details.format || undefined,
    quality: details.quality || undefined,
    title: details.title || undefined,
    pageUrl: details.pageUrl || undefined,
    origin: details.origin || undefined,
    acceptLanguage: details.acceptLanguage || navigator.languages?.join(",") || undefined,
  };
}

// ---- Capturing browser downloads ----

/** Takes over a browser download that matches the capture rules. */
async function captureDownload(item) {
  const url = item.finalUrl || item.url;
  if (bypassed.has(url) || bypassed.has(item.url)) {
    return;
  }
  const settings = await loadSettings();
  const size = [item.totalBytes, item.fileSize].find((bytes) => bytes > 0);
  const fileName = DownpourRules.baseName(item.filename);
  const decision = DownpourRules.shouldCapture({ url, fileName, sizeBytes: size }, settings);
  if (!decision.capture) {
    return;
  }

  // Stop the browser's copy first; it would otherwise race the app for the file.
  try {
    await browser.downloads.cancel(item.id);
  } catch {
    // The download already finished or failed; nothing left to take over.
    return;
  }
  // Removing the cancelled entry from the browser's list is cosmetic. The
  // browser can refuse while the item is still settling, so retry once a
  // moment later and never let it block the hand-off.
  eraseSoon(item.id);

  // The referrer is the page the download was started from; the app keeps
  // it to find a fresh link when the first one expires.
  const request = await buildItem(url, {
    fileName,
    referrer: item.referrer,
    pageUrl: item.referrer,
    mime: item.mime,
    size,
    cookieStoreId: item.cookieStoreId,
  });
  const reply = await sendDownloads([request]);
  const where = { sourcePage: item.referrer }; // the browser does not say which tab
  if (reply.type === "accepted") {
    announceHandOffLogged(where, DownpourRules.handOffNotice(reply, [request], { fallback: DownpourRules.REFUSAL_FALLBACK.NONE }));
    return;
  }
  const browserSaved = await fallBackToBrowser(item, reply.message);
  const fallback = browserSaved ? DownpourRules.REFUSAL_FALLBACK.BROWSER_SAVES : DownpourRules.REFUSAL_FALLBACK.BROWSER_FAILED;
  announceHandOffLogged(where, DownpourRules.handOffNotice(reply, [request], { fallback }));
}

function eraseSoon(downloadId) {
  const erase = () => browser.downloads.erase({ id: downloadId });
  erase().catch(() => {
    setTimeout(() => erase().catch((error) => console.warn("could not clear the cancelled download", error)), 1000);
  });
}

/** No capture takes `url` for a while; the browser saves it. */
function bypassCapture(url, reason) {
  bypassed.add(url);
  // Addresses can carry tokens, so the log names only the reason.
  console.warn(`Downpour leaves a download to the browser: ${reason}`);
}

/**
 * The app could not take the browser's download `item`: give it back to the
 * browser in the same private window or container. True when the browser
 * took it.
 */
async function fallBackToBrowser(item, reason) {
  const options = DownpourRules.browserFallbackDownload(item);
  bypassCapture(options.url, reason);
  try {
    await browser.downloads.download(options);
    return true;
  } catch (error) {
    console.error("browser fallback download failed", error);
    return false;
  }
}

// ---- Capturing page responses ----
// Firefox opens its downloads panel as soon as a download starts, before
// onCreated reaches us. So a page response that would become a download is
// stopped as its headers arrive, and the browser never starts its copy.
// onCreated still catches what this check misses.

// The blocking listener's answers: the browser goes on with the response, or drops it.
const LET_BROWSER_HANDLE = undefined;
const STOP_BROWSER_COPY = Object.freeze({ cancel: true });
// The tab waits on the app while a download is handed over. After this the
// browser saves the file itself. If the app still accepts it later, both
// download it; that is better than a tab that never finishes loading.
const HAND_OVER_TIMEOUT_MS = 10_000;

/**
 * Answers synchronously for ordinary pages, so they never wait on us; only a
 * response that is a download waits for the app's reply.
 */
function onPageResponse(details) {
  const sent = takeSentHeaders(details.requestId);
  if (details.tabId < 0 || bypassed.has(details.url)) {
    return LET_BROWSER_HANDLE;
  }
  const decision = DownpourRules.pageResponseDecision(details);
  if (decision.action === DownpourRules.PAGE_RESPONSE.SHOWN) {
    return LET_BROWSER_HANDLE;
  }
  if (decision.action === DownpourRules.PAGE_RESPONSE.BROWSER_KEEPS) {
    // onCreated would otherwise cancel the browser's copy and send the app
    // an address it cannot fetch.
    bypassCapture(details.url, decision.reason);
    return LET_BROWSER_HANDLE;
  }
  return handOverResponse(details, decision.download, sent).catch((error) => {
    console.error("capture failed", error);
    return LET_BROWSER_HANDLE;
  });
}

async function sendDownloadInTime(request) {
  let timer;
  const late = new Promise((resolve) => {
    timer = setTimeout(() => resolve({ type: "error", message: "Downpour did not answer in time." }), HAND_OVER_TIMEOUT_MS);
  });
  try {
    return await Promise.race([sendDownloads([request]), late]);
  } finally {
    clearTimeout(timer);
  }
}

async function handOverResponse(details, download, sent) {
  const settings = await loadSettings();
  const decision = DownpourRules.shouldCapture(
    { url: details.url, fileName: download.fileName, sizeBytes: download.sizeBytes ?? undefined },
    settings,
  );
  if (!decision.capture) {
    return LET_BROWSER_HANDLE;
  }
  // originUrl is the page that started the navigation, for when no Referer
  // was sent; a navigation started by an add-on has no page.
  const startingPage = /^https?:/i.test(details.originUrl ?? "") ? details.originUrl : undefined;
  const referrer = sent.referer || startingPage;
  const request = await buildItem(details.url, {
    fileName: download.fileName,
    referrer,
    pageUrl: referrer,
    mime: download.mime,
    size: download.sizeBytes ?? undefined,
    origin: sent.origin,
    acceptLanguage: sent.acceptLanguage,
    cookieStoreId: details.cookieStoreId,
  });
  const reply = await sendDownloadInTime(request);
  // Refused, the response goes on to the browser, which saves it.
  const notice = DownpourRules.handOffNotice(reply, [request], { fallback: DownpourRules.REFUSAL_FALLBACK.BROWSER_SAVES });
  const where = { tabId: details.tabId, sourcePage: referrer };
  if (reply.type !== "accepted") {
    bypassCapture(details.url, reply.message);
    announceHandOffLogged(where, notice);
    return LET_BROWSER_HANDLE;
  }
  // On a later task, so the tab is closed only after Firefox has our answer
  // for the request it is still holding open in that tab. The notice then
  // goes to the page the user is looking at.
  setTimeout(async () => {
    if (details.type === "main_frame") {
      await closeTabIfEmpty(details.tabId).catch((error) => console.warn("could not close the empty tab", error));
    }
    announceHandOffLogged(where, notice);
  }, 0);
  return STOP_BROWSER_COPY;
}

/**
 * Firefox closes a tab that was opened only for a download; with the
 * download stopped, the add-on does the same. A tab that shows a page keeps
 * showing it.
 */
async function closeTabIfEmpty(tabId) {
  const frame = await browser.webNavigation.getFrame({ tabId, frameId: 0 });
  if (frame?.url === "about:blank") {
    await browser.tabs.remove(tabId);
  }
}

// ---- Hand-off notice ----
// A download handed to the app leaves no trace in the browser, so the page
// it came from shows a short notice in its lower right corner, and the
// toolbar icon a check mark (also where no notice is drawn).

// How long the notice and the icon's mark stay.
const NOTICE_MS = 4000;
// The toolbar icon's badge: coral for detected media and downloads sent,
// yellow when the app refused one. Matches the popup's palette.
const BADGE_COUNT_LOOK = Object.freeze({ background: "#F8615A", text: "#FFFFFF" });
const BADGE_LOOK = Object.freeze({
  sent: Object.freeze({ ...BADGE_COUNT_LOOK, mark: "✓" }),
  refused: Object.freeze({ background: "#F6E04B", text: "#2F3238", mark: "!" }),
});
const badgeTimers = new Map(); // tab id -> the timer that restores its badge

/**
 * Runs in the page: shows `notice` in the lower right corner for
 * `visibleMs`. Notices that come quickly stack. The shadow root is closed and
 * its stack kept in the add-on's own script world (which lasts across
 * injections into this page), so the page's scripts can neither restyle nor
 * read the notice.
 */
function showHandOffNotice(notice, visibleMs) {
  const LEAVE_MS = 240;
  function createStack() {
    const host = document.createElement("downpour-handoff-notice");
    const shadow = host.attachShadow({ mode: "closed" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; position: fixed; right: 16px; bottom: 16px; z-index: 2147483647; }
        .stack { display: flex; flex-direction: column; align-items: flex-end; gap: 8px; }
        .notice {
          display: flex; align-items: center; gap: 10px; max-width: 340px; padding: 10px 14px 10px 10px;
          border-radius: 14px; background: rgba(64, 68, 75, 0.97); color: #fff; box-shadow: 0 10px 28px rgba(0, 0, 0, 0.4);
          font: 500 13px/1.3 "Segoe UI", system-ui, sans-serif; animation: enter 180ms ease-out;
        }
        .notice.leaving { opacity: 0; transform: translateY(6px); transition: opacity 220ms ease, transform 220ms ease; }
        .mark { flex: none; display: grid; place-items: center; width: 22px; height: 22px; border-radius: 50%; background: #f8615a; color: #fff; }
        .notice[data-outcome="refused"] .mark { background: #f6e04b; color: #2f3238; }
        .mark svg { width: 13px; height: 13px; }
        .text { min-width: 0; }
        .title { font-weight: 700; }
        .detail { color: #b4b8c0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .detail:empty { display: none; }
        @keyframes enter { from { opacity: 0; transform: translateY(8px); } }
        @media (prefers-reduced-motion: reduce) { .notice { animation: none; } .notice.leaving { transition: none; } }
      </style>
      <div class="stack" role="status" aria-live="polite"></div>`;
    return { host, list: shadow.querySelector(".stack") };
  }
  const stack = (globalThis.downpourNoticeStack ??= createStack());
  if (!stack.host.isConnected) (document.body ?? document.documentElement).append(stack.host);

  const SENT = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 8.5 6.5 11.5 12.5 4.5"/></svg>`;
  const REFUSED = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M8 3.5v5.5M8 12.2v.1"/></svg>`;
  const item = document.createElement("div");
  item.className = "notice";
  item.dataset.outcome = notice.outcome;
  item.innerHTML = `<span class="mark">${notice.outcome === "sent" ? SENT : REFUSED}</span><span class="text"><div class="title"></div><div class="detail"></div></span>`;
  item.querySelector(".title").textContent = notice.title;
  item.querySelector(".detail").textContent = notice.detail;
  item.title = notice.detail;
  stack.list.append(item);
  setTimeout(() => {
    item.classList.add("leaving");
    setTimeout(() => {
      item.remove();
      if (!stack.list.firstElementChild) stack.host.remove();
    }, LEAVE_MS);
  }, visibleMs);
}

/**
 * Where to tell the user about a download from `sourcePage`: the tab
 * `tabId` while it shows a page, else the active tab. The icon's mark goes
 * to that tab; the notice, which names the file, is drawn only when the tab
 * shows the site the download came from, never into an unrelated site.
 * @returns {Promise<{tab: object, drawable: boolean} | null>}
 */
async function noticeTargetFor(tabId, sourcePage) {
  let tab = null;
  if (tabId !== undefined && tabId >= 0) {
    tab = await browser.tabs.get(tabId).catch(() => null); // closed meanwhile
  }
  if (!tab || tab.url === "about:blank") {
    [tab] = await browser.tabs.query({ active: true, lastFocusedWindow: true });
  }
  if (!tab) return null;
  return { tab, drawable: DownpourRules.sameOrigin(tab.url, sourcePage) };
}

async function restoreBadge(tabId) {
  badgeTimers.delete(tabId);
  const tab = await browser.tabs.get(tabId).catch(() => null);
  if (!tab) return; // closed: nothing to restore
  await tabMedia.inspect(tabId, (media) => updateBadge(tabId, media.length));
}

async function flashBadge(tabId, outcome) {
  const look = BADGE_LOOK[outcome];
  await browser.action.setBadgeBackgroundColor({ color: look.background, tabId });
  await browser.action.setBadgeTextColor({ color: look.text, tabId });
  await browser.action.setBadgeText({ text: look.mark, tabId });
  // A newer mark gets the full time; the older one's restore is dropped.
  clearTimeout(badgeTimers.get(tabId));
  badgeTimers.set(
    tabId,
    setTimeout(() => {
      restoreBadge(tabId).catch((error) => console.warn("could not restore the badge", error));
    }, NOTICE_MS),
  );
}

/**
 * Tells the user how a hand-off went: a mark on the toolbar icon, and the
 * notice in the page when it is the download's own site. Browser pages and
 * the add-on store cannot be drawn on and keep just the mark.
 * @param {{tabId?: number, sourcePage?: string}} where the download's tab
 *   and the page it came from
 */
async function announceHandOff(where, notice) {
  const target = await noticeTargetFor(where.tabId, where.sourcePage);
  if (!target) return;
  await flashBadge(target.tab.id, notice.outcome);
  const { handOffNotice } = await loadSettings();
  if (!handOffNotice || !target.drawable) return;
  try {
    await browser.scripting.executeScript({ target: { tabId: target.tab.id }, func: showHandOffNotice, args: [notice, NOTICE_MS] });
  } catch {
    // Pages the add-on may not draw on (browser pages, PDFs) keep the mark.
  }
}

function announceHandOffLogged(where, notice) {
  announceHandOff(where, notice).catch((error) => console.warn("could not show the hand-off notice", error));
}

/** Sends `items` from the page in `tab` and tells the user how it went. */
async function sendDownloadsAnnounced(items, tab) {
  const reply = await sendDownloads(items);
  const notice = DownpourRules.handOffNotice(reply, items, { fallback: DownpourRules.REFUSAL_FALLBACK.NONE });
  announceHandOffLogged({ tabId: tab?.id, sourcePage: tab?.url }, notice);
  return reply;
}

// ---- Context menus ----

// A blob: player's real source is the stream detected in the tab; until one
// is found there is nothing to send.
const NO_STREAM_MESSAGE = "No stream found yet. Play the video for a moment, then try again.";

async function createMenus() {
  await browser.contextMenus.removeAll();
  browser.contextMenus.create({ id: MENU.link, title: "Download link with Downpour", contexts: ["link"] });
  browser.contextMenus.create({ id: MENU.media, title: "Download media with Downpour", contexts: ["image", "video", "audio"] });
  browser.contextMenus.create({ id: MENU.page, title: "Download all links with Downpour", contexts: ["page"] });
}

/** Runs in the page: collects unique http(s) links from anchors. */
function collectPageLinks() {
  const seen = new Set();
  for (const anchor of document.querySelectorAll("a[href]")) {
    const href = anchor.href;
    if (/^https?:/i.test(href) && !href.includes("#")) {
      seen.add(href);
    }
  }
  return [...seen];
}

async function onMenuClicked(info, tab) {
  const referrer = info.pageUrl || tab?.url;
  if (info.menuItemId === MENU.link && info.linkUrl) {
    await sendDownloadsAnnounced([await buildItem(info.linkUrl, { referrer, pageUrl: referrer, title: tab?.title, cookieStoreId: tab?.cookieStoreId })], tab);
  } else if (info.menuItemId === MENU.media && info.srcUrl) {
    // Streaming players play from a blob: URL; the real source is the
    // playlist detected in the tab.
    const isBlob = /^blob:/i.test(info.srcUrl);
    if (!isBlob) {
      const details = { referrer, pageUrl: tab?.url, title: tab?.title, cookieStoreId: tab?.cookieStoreId };
      await sendDownloadsAnnounced([await buildItem(info.srcUrl, details)], tab);
      return;
    }
    const stream = tab?.id !== undefined ? await latestStream(tab.id) : undefined;
    if (!stream) {
      const notice = DownpourRules.handOffNotice({ type: "error", message: NO_STREAM_MESSAGE }, [], {
        fallback: DownpourRules.REFUSAL_FALLBACK.NONE,
      });
      announceHandOffLogged({ tabId: tab?.id, sourcePage: tab?.url }, notice);
      return;
    }
    await sendDownloadsAnnounced([await buildItem(stream.url, stream)], tab);
  } else if (info.menuItemId === MENU.page && tab?.id !== undefined) {
    const [result] = await browser.scripting.executeScript({ target: { tabId: tab.id }, func: collectPageLinks });
    const settings = await loadSettings();
    // Only links that look like files; a page has many navigation links.
    const links = (result?.result ?? []).filter((url) => DownpourRules.shouldCapture({ url }, { ...settings, captureEnabled: true }).capture);
    if (links.length > 0) {
      const details = { referrer, pageUrl: referrer, cookieStoreId: tab?.cookieStoreId };
      await sendDownloadsAnnounced(await Promise.all(links.map((url) => buildItem(url, details))), tab);
    }
  }
}

// ---- Media detection ----

// Detected media is per tab and per browser session.
const mediaStore = browser.storage.session;

function mediaKey(tabId) {
  return `media:${tabId}`;
}

async function mediaForTab(tabId) {
  const key = mediaKey(tabId);
  const stored = await mediaStore.get(key);
  return stored[key] ?? [];
}

async function tabExists(tabId) {
  try {
    await browser.tabs.get(tabId);
    return true;
  } catch {
    return false; // tabs.get rejects only for a tab that is gone
  }
}

/**
 * Stores a tab's list and shows its count on the badge. Both happen inside
 * the ledger's queue, so the badge always matches the list last written.
 */
async function writeTabMedia(tabId, list) {
  if (list.length === 0) {
    await mediaStore.remove(mediaKey(tabId));
  } else {
    await mediaStore.set({ [mediaKey(tabId)]: list });
  }
  try {
    await updateBadge(tabId, list.length);
  } catch (error) {
    // A closed tab has no badge left to set; any other failure is real.
    if (await tabExists(tabId)) {
      throw error;
    }
  }
}

// Detections overlap (a player loads several playlists at once, and a
// playlist read can take seconds), so every change to a tab's list goes
// through the ledger.
const tabMedia = DownpourRules.createTabMediaLedger({ read: mediaForTab, write: writeTabMedia });

// ---- Headers the page sent ----
// Video CDNs often check Referer, Origin, or Accept-Language on every piece
// of a stream. These are remembered per request, briefly, so the app can
// send the same ones.

const SENT_HEADERS_TTL_MS = 60_000;
const sentHeaders = new Map();

function headerOf(headers, name) {
  return DownpourRules.headerValue(headers, name) || undefined;
}

function onSendHeaders(details) {
  if (details.tabId < 0) {
    return;
  }
  const now = Date.now();
  for (const [id, entry] of sentHeaders) {
    if (now - entry.at > SENT_HEADERS_TTL_MS) {
      sentHeaders.delete(id);
    }
  }
  sentHeaders.set(details.requestId, {
    at: now,
    referer: headerOf(details.requestHeaders, "referer"),
    origin: headerOf(details.requestHeaders, "origin"),
    acceptLanguage: headerOf(details.requestHeaders, "accept-language"),
  });
}

function takeSentHeaders(requestId) {
  const entry = sentHeaders.get(requestId) ?? {};
  sentHeaders.delete(requestId);
  return entry;
}

// ---- Media and stream detection ----

const PLAYLIST_FETCH_TIMEOUT_MS = 5_000;
const MAX_PLAYLIST_CHARS = 2_000_000;

async function onHeadersReceived(details) {
  if (details.tabId < 0) {
    return;
  }
  const okay = details.statusCode === 200 || details.statusCode === 206;
  const format = okay ? DownpourRules.streamFormatOf(details.url, details.responseHeaders) : null;
  if (format) {
    await recordStream(details, format);
    return;
  }
  takeSentHeaders(details.requestId);
  const media = DownpourRules.mediaFromResponse(details.responseHeaders, details.statusCode, details.url);
  if (!media) {
    return;
  }
  const ticket = tabMedia.begin(details.tabId);
  const page = await pageOf(details.tabId);
  await addTabMedia(ticket, {
    kind: "file",
    url: details.url,
    mime: media.mime,
    size: media.sizeBytes,
    fileName: DownpourRules.fileNameFromUrl(details.url),
    referrer: details.documentUrl || details.initiator || undefined,
    pageUrl: page.url,
    frameId: details.frameId,
    cookieStoreId: details.cookieStoreId,
  });
}

/** Lists an HLS or DASH stream once, hiding the variant playlists it lists. */
async function recordStream(details, format) {
  const sent = takeSentHeaders(details.requestId);
  const tabId = details.tabId;
  const ticket = tabMedia.begin(tabId);
  // A first look before the slow playlist read; the ledger checks again.
  if (DownpourRules.isListedMedia(await mediaForTab(tabId), details.url)) {
    return;
  }
  const playlist = await readPlaylist(details.url, details.cookieStoreId);
  const page = await pageOf(tabId);
  await addTabMedia(ticket, {
    kind: "stream",
    format,
    url: details.url,
    playlistKind: playlist.kind,
    heights: playlist.heights,
    // Only a multivariant playlist's entries are other playlists to hide.
    variantUrls: playlist.kind === "multivariant" ? playlist.variantUrls : [],
    title: page.title,
    pageUrl: page.url,
    referrer: sent.referer || details.documentUrl || details.initiator || page.url,
    origin: sent.origin,
    acceptLanguage: sent.acceptLanguage,
    // Which frame's player loaded it, for the on-video Download button.
    frameId: details.frameId,
    cookieStoreId: details.cookieStoreId,
  });
}

async function addTabMedia(ticket, entry) {
  await tabMedia.update(ticket, (list) => DownpourRules.withDetectedMedia(list, entry, MAX_MEDIA_PER_TAB));
}

/**
 * Best effort: a playlist that needs the page's Referer may refuse us. The
 * background page fetches with the normal window's cookies only, so a
 * playlist from a private window or container is not read; its stream is
 * still listed, without its qualities.
 */
async function readPlaylist(url, cookieStoreId) {
  if (!DownpourRules.isDefaultCookieStore(cookieStoreId)) {
    return DownpourRules.describePlaylist("", url);
  }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), PLAYLIST_FETCH_TIMEOUT_MS);
  try {
    const response = await fetch(url, { credentials: "include", signal: controller.signal });
    if (!response.ok) {
      return DownpourRules.describePlaylist("", url);
    }
    const text = (await response.text()).slice(0, MAX_PLAYLIST_CHARS);
    return DownpourRules.describePlaylist(text, response.url || url);
  } catch {
    return DownpourRules.describePlaylist("", url);
  } finally {
    clearTimeout(timer);
  }
}

async function pageOf(tabId) {
  try {
    const tab = await browser.tabs.get(tabId);
    return { title: tab.title || undefined, url: tab.url || undefined };
  } catch {
    return {};
  }
}

/** The newest stream found in the tab, for players whose `src` is a blob. */
async function latestStream(tabId) {
  return (await mediaForTab(tabId)).find((entry) => entry.kind === "stream");
}

async function updateBadge(tabId, count) {
  await browser.action.setBadgeBackgroundColor({ color: BADGE_COUNT_LOOK.background, tabId });
  await browser.action.setBadgeTextColor({ color: BADGE_COUNT_LOOK.text, tabId });
  await browser.action.setBadgeText({ text: count > 0 ? String(Math.min(count, 99)) : "", tabId });
}

// ---- Messages from the popup and the video button ----

/** What the video in the sender's frame plays, or null when nothing is known yet. */
async function videoSourceFor(message, sender) {
  const tabId = sender?.tab?.id;
  if (tabId === undefined) {
    return null;
  }
  const entries = await mediaForTab(tabId);
  return DownpourRules.pickVideoSource(entries, { src: message.src, frameId: sender.frameId ?? 0 });
}

/** The on-video button's menu: the qualities the clicked video's stream offers. */
async function videoQualities(message, sender) {
  const source = await videoSourceFor(message, sender);
  return { type: "qualities", choices: DownpourRules.videoQualityChoices(source?.heights) };
}

/** The on-video Download button: sends what the clicked video plays. */
async function downloadVideo(message, sender) {
  if (sender?.tab?.id === undefined) {
    return { type: "error", message: "This page is not in a tab." };
  }
  const picked = DownpourRules.parseQuality(message.quality);
  if (!picked.ok) {
    return { type: "error", message: picked.reason };
  }
  const source = await videoSourceFor(message, sender);
  if (!source) {
    return { type: "error", message: "No video found yet. Play it for a moment, then try again." };
  }
  const page = await pageOf(sender.tab.id);
  // A frame without its own address (about:blank) is not a usable referrer.
  const frameUrl = /^https?:/i.test(sender.url ?? "") ? sender.url : page.url;
  return sendDownloads([
    await buildItem(source.url, {
      ...source,
      // A plain video file has one quality; only streams have a choice.
      quality: source.kind === "stream" ? picked.quality : undefined,
      referrer: source.referrer || frameUrl,
      title: source.title || page.title,
      pageUrl: source.pageUrl || page.url,
      cookieStoreId: sender.tab.cookieStoreId,
    }),
  ]);
}

/** The popup is an extension page: our own id and no tab. */
function isFromPopup(sender) {
  return sender?.id === browser.runtime.id && !sender.tab;
}

/** The on-video button runs as our content script in a tab. */
function isFromOurContentScript(sender) {
  return sender?.id === browser.runtime.id && Boolean(sender.tab);
}

function refuse(message) {
  return { type: "error", message };
}

/** The link the popup's form sends, with the cookies of the popup's tab. */
async function downloadUrl(message) {
  const parsed = DownpourRules.parseDownloadUrl(message.url);
  if (!parsed.ok) {
    return refuse(parsed.reason);
  }
  const tab = Number.isInteger(message.tabId) ? await browser.tabs.get(message.tabId).catch(() => null) : null;
  const referrer = DownpourRules.parseDownloadUrl(tab?.url).ok ? tab.url : undefined;
  return sendDownloads([await buildItem(parsed.url, { referrer, cookieStoreId: tab?.cookieStoreId })]);
}

/** One of the tab's detected media, as listed; nothing the popup did not see. */
async function downloadMedia(message) {
  if (!Number.isInteger(message.tabId)) {
    return refuse("The popup did not say which tab.");
  }
  const entry = (await mediaForTab(message.tabId)).find((media) => media.url === message.url);
  if (!entry) {
    return refuse("That media is no longer listed. Reopen the popup.");
  }
  return sendDownloads([await buildItem(entry.url, entry)]);
}

async function tabMediaList(message) {
  if (!Number.isInteger(message.tabId)) {
    return refuse("The popup did not say which tab.");
  }
  return { type: "media", items: await mediaForTab(message.tabId) };
}

const POPUP_REQUESTS = Object.freeze({
  status: () => sendToHost({ type: "ping" }),
  show: () => sendToHost({ type: "show" }),
  "download-url": downloadUrl,
  "download-media": downloadMedia,
  "tab-media": tabMediaList,
});

const CONTENT_SCRIPT_REQUESTS = Object.freeze({
  "download-video": downloadVideo,
  "video-qualities": videoQualities,
});

/**
 * Answers the popup and the on-video button. Each request is served only to
 * the part of the add-on that sends it.
 */
async function onExtensionMessage(message, sender) {
  const type = typeof message?.type === "string" ? message.type : "";
  if (Object.hasOwn(POPUP_REQUESTS, type)) {
    return isFromPopup(sender) ? POPUP_REQUESTS[type](message) : refuse(`${type} is answered only to the popup`);
  }
  if (Object.hasOwn(CONTENT_SCRIPT_REQUESTS, type)) {
    return isFromOurContentScript(sender)
      ? CONTENT_SCRIPT_REQUESTS[type](message, sender)
      : refuse(`${type} is answered only to the video button`);
  }
  return refuse(`unknown request ${type || "(none)"}`);
}

// ---- Wiring ----
// Listeners are registered synchronously at top level so the browser can wake
// the background script for these events.

function createMenusLogged() {
  createMenus().catch((error) => console.error("creating menus failed", error));
}

browser.runtime.onInstalled.addListener(createMenusLogged);
browser.runtime.onStartup.addListener(createMenusLogged);
browser.contextMenus.onClicked.addListener((info, tab) => {
  onMenuClicked(info, tab).catch((error) => console.error("menu action failed", error));
});
browser.downloads.onCreated.addListener((item) => {
  captureDownload(item).catch((error) => console.error("capture failed", error));
});
const PAGE_TYPES = ["main_frame", "sub_frame"];
browser.webRequest.onSendHeaders.addListener(
  onSendHeaders,
  { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "other", ...PAGE_TYPES] },
  ["requestHeaders"],
);
browser.webRequest.onHeadersReceived.addListener(
  onPageResponse,
  { urls: ["http://*/*", "https://*/*"], types: PAGE_TYPES },
  ["blocking", "responseHeaders"],
);
browser.webRequest.onHeadersReceived.addListener(
  (details) => {
    onHeadersReceived(details).catch((error) => console.error("media detection failed", error));
  },
  { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "other", "object"] },
  ["responseHeaders"],
);
browser.webNavigation.onCommitted.addListener((details) => {
  if (details.frameId === 0) {
    tabMedia.reset(details.tabId).catch((error) => console.warn("could not clear the tab's media", error));
  }
});
browser.tabs.onRemoved.addListener((tabId) => {
  tabMedia.reset(tabId).catch((error) => console.warn("could not clear the closed tab's media", error));
});
browser.runtime.onMessage.addListener((message, sender, sendResponse) => {
  onExtensionMessage(message, sender)
    .then(sendResponse)
    .catch((error) => sendResponse({ type: "error", message: String(error?.message ?? error) }));
  return true; // keeps the channel open for the async reply
});
