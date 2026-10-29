const fields = {
  form: document.getElementById("options"),
  capture: document.getElementById("capture"),
  videoButton: document.getElementById("video-button"),
  handOffNotice: document.getElementById("handoff-notice"),
  types: document.getElementById("types"),
  size: document.getElementById("size"),
  excluded: document.getElementById("excluded"),
  reset: document.getElementById("reset"),
  saved: document.getElementById("saved"),
  failed: document.getElementById("failed"),
};

function show(settings) {
  fields.capture.checked = settings.captureEnabled;
  fields.videoButton.checked = settings.videoButton !== false;
  fields.handOffNotice.checked = settings.handOffNotice !== false;
  fields.types.value = DownpourRules.parseList(settings.fileTypes).join(" ");
  fields.size.value = settings.minSizeKb;
  fields.excluded.value = DownpourRules.parseList(settings.excludedSites).join("\n");
}

function read() {
  const size = Number.parseInt(fields.size.value, 10);
  return {
    captureEnabled: fields.capture.checked,
    videoButton: fields.videoButton.checked,
    handOffNotice: fields.handOffNotice.checked,
    fileTypes: DownpourRules.parseList(fields.types.value).join(" "),
    minSizeKb: Number.isFinite(size) && size > 0 ? size : 0,
    excludedSites: DownpourRules.parseList(fields.excluded.value).join(" "),
  };
}

function showFailure(what, error) {
  fields.saved.hidden = true;
  fields.failed.textContent = `${what}: ${error?.message ?? error}`;
  fields.failed.hidden = false;
}

async function save(settings) {
  fields.failed.hidden = true;
  try {
    await browser.storage.local.set(settings);
  } catch (error) {
    showFailure("Could not save", error);
    return;
  }
  show(settings);
  fields.saved.hidden = false;
  setTimeout(() => {
    fields.saved.hidden = true;
  }, 1800);
}

fields.form.addEventListener("submit", (event) => {
  event.preventDefault();
  save(read());
});

fields.reset.addEventListener("click", () => save({ ...DownpourRules.DEFAULT_SETTINGS }));

browser.storage.local
  .get(DownpourRules.DEFAULT_SETTINGS)
  .then(show)
  .catch((error) => showFailure("Could not load the settings", error));
