// Content script: shows a small Download button over videos. A click
// downloads the best quality; the arrow next to it
// lists the other qualities of a stream. Runs in every frame, because
// players are often embedded in iframes.
//
// Players usually cover the <video> with control layers that receive the
// mouse events, so the button follows the pointer position across the whole
// window and checks which video lies under it, instead of relying on hover
// events of the video element.

(() => {
  // Smaller videos are previews, ads, or thumbnails.
  const MIN_WIDTH = 200;
  const MIN_HEIGHT = 120;
  const HIDE_DELAY_MS = 1500;
  const NOTICE_MS = 2600;
  const EDGE_GAP = 8;
  // Custom players nest their <video> in shadow roots, sometimes in layers.
  const MAX_SHADOW_DEPTH = 4;
  const TITLE = "Download the best quality with Downpour";

  let enabled = true;
  let current = null; // the video the button is shown for
  let hideTimer = null;
  let host = null;
  let parts = null; // the shadow DOM's elements, once created
  // What the button shows for `current`; the DOM is drawn from these in
  // `render` and never read back. `status` is one of
  //   { kind: "idle" }, { kind: "busy" },
  //   { kind: "notice", outcome: "done" | "error", message }.
  let status = { kind: "idle" };
  let choices = []; // the stream's qualities, best first
  let menuOpen = false;
  let noticeTimer = null;
  // The latest pointer position, checked once per animation frame.
  let pointer = null;
  let checkScheduled = false;

  // The button is on until the stored setting says otherwise, so the first
  // mouse moves are never missed while storage is read.
  browser.storage.local
    .get({ videoButton: true })
    .then((stored) => {
      enabled = stored.videoButton !== false;
      if (!enabled) hide();
    })
    .catch((error) => console.warn("Downpour: could not read the video button setting; keeping it on", error));

  browser.storage.onChanged?.addListener((changes) => {
    if ("videoButton" in changes) {
      enabled = changes.videoButton.newValue !== false;
      if (!enabled) hide();
    }
  });

  // Only size counts. Players often keep the <video> transparent or hidden
  // under a poster until it plays, and the button should be there already.
  function isLarge(video) {
    if (!video.isConnected) return false;
    const rect = video.getBoundingClientRect();
    return rect.width >= MIN_WIDTH && rect.height >= MIN_HEIGHT;
  }

  function contains(rect, x, y) {
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }

  /** The large video under the point, looked up fresh (players replace their elements). */
  function videoAt(x, y) {
    for (const video of document.getElementsByTagName("video")) {
      if (contains(video.getBoundingClientRect(), x, y) && isLarge(video)) return video;
    }
    return videoInShadowAt(document, x, y, 0);
  }

  /**
   * A video inside the shadow roots of the elements at the point.
   * `getElementsByTagName` does not see into shadow roots; hit testing finds
   * the few hosts actually under the pointer instead of walking the page.
   */
  function videoInShadowAt(scope, x, y, depth) {
    if (depth >= MAX_SHADOW_DEPTH) return null;
    for (const element of scope.elementsFromPoint(x, y)) {
      // Firefox lets content scripts into closed shadow roots as well.
      const shadow = element.openOrClosedShadowRoot ?? element.shadowRoot;
      if (!shadow || element === host) continue;
      for (const video of shadow.querySelectorAll("video")) {
        if (contains(video.getBoundingClientRect(), x, y) && isLarge(video)) return video;
      }
      const nested = videoInShadowAt(shadow, x, y, depth + 1);
      if (nested) return nested;
    }
    return null;
  }

  const ICON_DOWNLOAD = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.5v7.5M4.5 6.8 8 10.3l3.5-3.5M3 13.5h10"/></svg>`;
  const ICON_DONE = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 8.5 6.5 11.5 12.5 4.5"/></svg>`;
  const ICON_ERROR = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><path d="M8 3.5v5.5M8 12.2v.1"/></svg>`;
  const ICON_BUSY = `<svg class="spin" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M8 2.5a5.5 5.5 0 1 1-5.5 5.5"/></svg>`;
  const ICONS = { idle: ICON_DOWNLOAD, busy: ICON_BUSY, done: ICON_DONE, error: ICON_ERROR };

  function ensureButton() {
    if (host) return;
    host = document.createElement("downpour-video-button");
    const shadow = host.attachShadow({ mode: "open" });
    shadow.innerHTML = `
      <style>
        :host { all: initial; position: fixed; z-index: 2147483647; top: 0; left: 0; display: none; }
        .bar {
          display: inline-flex; align-items: stretch; height: 26px; border-radius: 13px; overflow: hidden;
          font: 600 11.5px/1 "Segoe UI", system-ui, sans-serif; color: #fff; white-space: nowrap;
          background: rgba(61, 65, 72, 0.88); box-shadow: 0 4px 12px rgba(0, 0, 0, 0.35);
          backdrop-filter: blur(6px);
        }
        button {
          all: initial; box-sizing: border-box; display: inline-flex; align-items: center; gap: 5px;
          cursor: pointer; color: inherit; font: inherit; transition: background 120ms ease;
        }
        button:hover, button[aria-expanded="true"] { background: #f8615a; }
        .main { padding: 0 8px; }
        .label:empty { display: none; }
        .more { padding: 0 6px 0 5px; border-left: 1px solid rgba(255, 255, 255, 0.18); }
        .more[hidden] { display: none; }
        .bar[data-state="busy"] .main { cursor: progress; }
        .bar[data-state="done"] { background: #f8615a; }
        .bar[data-state="error"] { background: #f6e04b; color: #2f3238; }
        svg { width: 14px; height: 14px; flex: none; display: block; }
        .more svg { width: 10px; height: 10px; }
        .spin { animation: spin 0.8s linear infinite; }
        @keyframes spin { to { transform: rotate(360deg); } }
        .menu, .notice {
          position: absolute; top: 30px; right: 0; min-width: 112px; padding: 4px;
          border-radius: 10px; background: rgba(64, 68, 75, 0.97); box-shadow: 0 8px 22px rgba(0, 0, 0, 0.4);
          font: 500 12px/1.2 "Segoe UI", system-ui, sans-serif; color: #fff;
        }
        .menu[hidden], .notice[hidden] { display: none; }
        .menu button { display: flex; width: 100%; padding: 6px 10px; border-radius: 999px; }
        .notice { width: max-content; max-width: 240px; padding: 7px 10px; white-space: normal; }
      </style>
      <div class="bar" data-state="idle">
        <button class="main" type="button" title="${TITLE}"><span class="icon"></span><span class="label"></span></button>
        <button class="more" type="button" title="Choose the quality" aria-haspopup="menu" aria-expanded="false" hidden>
          <svg viewBox="0 0 10 10" fill="currentColor"><path d="M1.5 3.5h7L5 7.5z"/></svg>
        </button>
      </div>
      <div class="menu" role="menu" hidden></div>
      <div class="notice" role="status" hidden></div>`;
    parts = {
      bar: shadow.querySelector(".bar"),
      main: shadow.querySelector(".main"),
      icon: shadow.querySelector(".icon"),
      label: shadow.querySelector(".label"),
      more: shadow.querySelector(".more"),
      menu: shadow.querySelector(".menu"),
      notice: shadow.querySelector(".notice"),
    };
    parts.main.addEventListener("click", (event) => {
      stop(event);
      download(undefined); // the background reads no quality as the best
    });
    parts.more.addEventListener("click", (event) => {
      stop(event);
      if (menuOpen) closeMenu();
      else openMenu();
    });
    host.addEventListener("mouseenter", () => {
      clearTimeout(hideTimer);
      refreshChoices();
    });
    host.addEventListener("mouseleave", scheduleHide);
    render();
  }

  // Players treat clicks that reach them as play/pause.
  function stop(event) {
    event.preventDefault();
    event.stopPropagation();
  }

  /** Puts the button at the video's top-right corner, inside the viewport. */
  function place() {
    if (!current || !host) return;
    if (!isLarge(current)) {
      hide();
      return;
    }
    // In fullscreen only the fullscreen element is drawn; move inside it.
    const container = document.fullscreenElement ?? document.documentElement;
    if (host.parentNode !== container) container.append(host);
    const rect = current.getBoundingClientRect();
    const width = parts.bar.offsetWidth || 40;
    const top = Math.max(rect.top, fixedHeaderBottom(rect)) + EDGE_GAP;
    const left = Math.min(rect.right, window.innerWidth) - width - EDGE_GAP;
    host.style.transform = `translate(${Math.max(EDGE_GAP, left)}px, ${Math.max(EDGE_GAP, top)}px)`;
  }

  /**
   * Bottom edge of a site header fixed over the top of the video, so the
   * button is not hidden beneath it.
   */
  function fixedHeaderBottom(rect) {
    if (rect.top > 0) return 0;
    let bottom = 0;
    for (const element of document.elementsFromPoint(rect.left + rect.width / 2, 1)) {
      if (element === host || element === current || current.contains(element)) continue;
      const style = getComputedStyle(element);
      if (style.position === "fixed" || style.position === "sticky") {
        const box = element.getBoundingClientRect();
        if (box.height < window.innerHeight / 3) bottom = Math.max(bottom, box.bottom);
      }
    }
    return bottom;
  }

  function show(video) {
    if (!enabled) return;
    ensureButton();
    clearTimeout(hideTimer);
    if (current !== video) {
      current = video;
      clearTimeout(noticeTimer);
      status = { kind: "idle" };
      choices = [];
      menuOpen = false;
      refreshChoices();
    }
    host.style.display = "block";
    render();
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    if (menuOpen) return;
    hideTimer = setTimeout(hide, HIDE_DELAY_MS);
  }

  function hide() {
    clearTimeout(hideTimer);
    menuOpen = false;
    if (host) host.style.display = "none";
    current = null;
  }

  /** Draws the button from `status`, `choices`, and `menuOpen`. */
  function render() {
    if (!parts) return;
    const look = status.kind === "notice" ? status.outcome : status.kind;
    parts.bar.dataset.state = look;
    parts.icon.innerHTML = ICONS[look];
    parts.main.title = look === "done" ? "Sent to Downpour" : TITLE;
    const best = choices[0]?.height;
    parts.label.textContent = best ? `${best}p` : "";
    parts.more.hidden = choices.length < 2;
    parts.more.setAttribute("aria-expanded", String(menuOpen));
    parts.menu.hidden = !menuOpen;
    const notice = status.kind === "notice" && status.outcome === "error" ? status.message : "";
    parts.notice.textContent = notice;
    parts.notice.hidden = !notice || menuOpen;
    place();
  }

  /**
   * Asks which qualities the video's stream has. A stream is often found
   * only after the player loads it, so this runs again when the pointer
   * comes back to the button.
   */
  async function refreshChoices() {
    const video = current;
    if (!video) return;
    let reply;
    try {
      reply = await browser.runtime.sendMessage({ type: "video-qualities", src: sourceOf(video) });
    } catch (error) {
      // The button still downloads the best quality, just without a menu.
      console.warn("Downpour: could not list the video's qualities", error);
      return;
    }
    if (video !== current || !Array.isArray(reply?.choices)) return;
    choices = reply.choices;
    if (choices.length < 2) menuOpen = false;
    render();
  }

  function openMenu() {
    if (!parts || choices.length < 2) return;
    parts.menu.replaceChildren(
      ...choices.map((choice) => {
        const item = document.createElement("button");
        item.type = "button";
        item.setAttribute("role", "menuitem");
        item.textContent = choice.label;
        item.addEventListener("click", (event) => {
          stop(event);
          closeMenu();
          download(choice.quality);
        });
        return item;
      }),
    );
    menuOpen = true;
    clearTimeout(hideTimer);
    render();
  }

  function closeMenu() {
    if (!menuOpen) return;
    menuOpen = false;
    render();
  }

  function sourceOf(video) {
    return video.currentSrc || video.src;
  }

  /** Sends the video to the app; `quality` undefined means the best. */
  async function download(quality) {
    const video = current;
    if (!video || status.kind === "busy") return;
    clearTimeout(noticeTimer);
    status = { kind: "busy" };
    render();
    let reply;
    try {
      reply = await browser.runtime.sendMessage({ type: "download-video", src: sourceOf(video), quality });
    } catch (error) {
      reply = { type: "error", message: String(error?.message ?? error) };
    }
    // The pointer moved on to another video meanwhile; its button is not ours.
    if (video !== current) return;
    status = reply?.type === "accepted"
      ? { kind: "notice", outcome: "done", message: "" }
      : { kind: "notice", outcome: "error", message: reply?.message ?? "Downpour did not answer." };
    render();
    noticeTimer = setTimeout(() => {
      status = { kind: "idle" };
      render();
    }, NOTICE_MS);
  }

  function onPointerMove(event) {
    if (!enabled) return;
    if (host && event.composedPath?.().includes(host)) {
      clearTimeout(hideTimer);
      return;
    }
    pointer = { x: event.clientX, y: event.clientY };
    if (!checkScheduled) {
      checkScheduled = true;
      requestAnimationFrame(checkPointer);
    }
  }

  function checkPointer() {
    checkScheduled = false;
    if (!pointer) return;
    const video = videoAt(pointer.x, pointer.y);
    if (video) {
      show(video);
    } else if (current) {
      scheduleHide();
    }
  }

  // A click anywhere else closes the quality menu.
  function onPointerDown(event) {
    if (menuOpen && !event.composedPath?.().includes(host)) {
      closeMenu();
      scheduleHide();
    }
  }

  function onKeyDown(event) {
    if (menuOpen && event.key === "Escape") closeMenu();
  }

  // A video starting to play (e.g. autoplay) shows the button without hover.
  // Media events do not leave shadow roots, so this sees light-DOM videos.
  function onPlay(event) {
    if (enabled && event.target instanceof HTMLVideoElement && isLarge(event.target)) {
      show(event.target);
      scheduleHide();
    }
  }

  // A video that loads under a pointer that is not moving would otherwise
  // wait for the next mouse move; check the last known position again.
  function onVideoReady(event) {
    if (enabled && pointer && event.target instanceof HTMLVideoElement && !checkScheduled) {
      checkScheduled = true;
      requestAnimationFrame(checkPointer);
    }
  }

  // The manifest runs this script at document_start, so these listeners
  // exist before the page's own scripts and ads finish loading; nothing
  // touches the page until a video is found.
  window.addEventListener("mousemove", onPointerMove, { capture: true, passive: true });
  window.addEventListener("pointerdown", onPointerDown, { capture: true, passive: true });
  window.addEventListener("keydown", onKeyDown, { capture: true });
  document.addEventListener("play", onPlay, { capture: true });
  document.addEventListener("loadedmetadata", onVideoReady, { capture: true });
  document.addEventListener("playing", onVideoReady, { capture: true });
  window.addEventListener("scroll", place, { capture: true, passive: true });
  window.addEventListener("resize", place, { passive: true });
  document.addEventListener("fullscreenchange", place);
})();
