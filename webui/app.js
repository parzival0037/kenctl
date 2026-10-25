/* KenCtl for Linux — web UI (no build step, no framework). */

const $ = (id) => document.getElementById(id);

let server = null;   // last full /api/state snapshot
let draft = null;    // editable copy of the selected profile
let originalName = null;
let keys = [];
let btnKeys = [];       // [{code:"BTN_SIDE", label:"BTN_TOP_RIGHT"}, ...]
let btnLabels = {};     // code -> positional label
let keyKeys = [];
let toastTimer = null;
let recording = null;   // { steps: [], last: <monotonic ms> }

function el(tag, attrs = {}, ...kids) {
  const n = document.createElement(tag);
  let children = kids;
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined) continue;
    if (k === "class") n.className = v;
    else if (k === "text") n.textContent = v;
    else if (k === "value") n.value = v;
    else if (k === "checked") n.checked = !!v;
    else if (k === "selected") n.selected = !!v;
    else if (k === "children") children = children.concat(v);
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else n.setAttribute(k, v);
  }
  for (const kid of children.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    n.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return n;
}

function toast(message, kind = "ok") {
  const t = $("toast");
  t.textContent = message;
  t.className = `toast ${kind}`;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 4000);
}

async function api(path, method = "GET", body) {
  const opts = { method, headers: { Accept: "application/json" } };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  const res = await fetch(path, opts);
  const text = await res.text();
  if (!res.ok) throw new Error(text.trim() || `HTTP ${res.status}`);
  return text ? JSON.parse(text) : null;
}

const clone = (o) => JSON.parse(JSON.stringify(o));
const btnLabel = (code) => btnLabels[code] || code;

// Browser KeyboardEvent.code (physical key) -> Linux evdev key name.
const CODE_TO_EVDEV = (() => {
  const m = {
    Escape: "KEY_ESC", Backquote: "KEY_GRAVE", Minus: "KEY_MINUS", Equal: "KEY_EQUAL",
    Backspace: "KEY_BACKSPACE", Tab: "KEY_TAB", BracketLeft: "KEY_LEFTBRACE",
    BracketRight: "KEY_RIGHTBRACE", Backslash: "KEY_BACKSLASH", CapsLock: "KEY_CAPSLOCK",
    Semicolon: "KEY_SEMICOLON", Quote: "KEY_APOSTROPHE", Enter: "KEY_ENTER",
    Comma: "KEY_COMMA", Period: "KEY_DOT", Slash: "KEY_SLASH", Space: "KEY_SPACE",
    Insert: "KEY_INSERT", Delete: "KEY_DELETE", Home: "KEY_HOME", End: "KEY_END",
    PageUp: "KEY_PAGEUP", PageDown: "KEY_PAGEDOWN",
    ArrowLeft: "KEY_LEFT", ArrowRight: "KEY_RIGHT", ArrowUp: "KEY_UP", ArrowDown: "KEY_DOWN",
    ControlLeft: "KEY_LEFTCTRL", ControlRight: "KEY_RIGHTCTRL",
    ShiftLeft: "KEY_LEFTSHIFT", ShiftRight: "KEY_RIGHTSHIFT",
    AltLeft: "KEY_LEFTALT", AltRight: "KEY_RIGHTALT",
    MetaLeft: "KEY_LEFTMETA", MetaRight: "KEY_RIGHTMETA",
    NumLock: "KEY_NUMLOCK", ScrollLock: "KEY_SCROLLLOCK", PrintScreen: "KEY_SYSRQ",
    Pause: "KEY_PAUSE", NumpadEnter: "KEY_KPENTER", NumpadAdd: "KEY_KPPLUS",
    NumpadSubtract: "KEY_KPMINUS", NumpadMultiply: "KEY_KPASTERISK",
    NumpadDivide: "KEY_KPSLASH", NumpadDecimal: "KEY_KPDOT",
  };
  for (let i = 0; i < 26; i++) {
    const ch = String.fromCharCode(65 + i);
    m["Key" + ch] = "KEY_" + ch;
  }
  for (let i = 0; i < 10; i++) {
    m["Digit" + i] = "KEY_" + i;
    m["Numpad" + i] = "KEY_KP" + i;
  }
  for (let i = 1; i <= 12; i++) m["F" + i] = "KEY_F" + i;
  return m;
})();

const MOUSE_BUTTONS = { 0: "BTN_LEFT", 1: "BTN_MIDDLE", 2: "BTN_RIGHT", 3: "BTN_SIDE", 4: "BTN_EXTRA" };

const MODIFIER_CODES = new Set([
  "KEY_LEFTCTRL", "KEY_RIGHTCTRL", "KEY_LEFTSHIFT", "KEY_RIGHTSHIFT",
  "KEY_LEFTALT", "KEY_RIGHTALT", "KEY_LEFTMETA", "KEY_RIGHTMETA",
]);

// ------------------------------------------------------------------ polling

async function poll() {
  server = await api("/api/state");
  renderBadge();
  renderDevices();
  renderHardware();
}

// ---------------------------------------------------------------- rendering

function renderBadge() {
  $("store-path").textContent = server.store_path;
  const editing = draft && draft.name !== server.active_name ? ` · editing ${draft.name}` : "";
  $("active-pill").textContent = `Active: ${server.active_name}${editing}`;
  const running = server.engine_running;
  $("engine-dot").className = `dot ${running ? "on" : "off"}`;
  const note = $("engine-note");
  if (running) {
    note.textContent = "Input engine active — mapped changes apply as soon as you save.";
    note.className = "engine-note ok";
  } else {
    note.textContent =
      "Input is not being grabbed (dry-run, no device matched, or /dev/uinput is not granted yet). " +
      "Run ./install.sh as root, then `kenctl run`.";
    note.className = "engine-note";
  }
}

function renderDevices() {
  const box = $("devices");
  box.replaceChildren();
  if (!server.devices.length) {
    box.append(el("p", { class: "empty", text: "No supported Kensington input nodes found." }));
    return;
  }
  for (const d of server.devices) {
    box.append(
      el("div", { class: "device", id: "dev-" + d.path.replace(/\W/g, "_") },
        el("span", { class: `tag ${d.managed ? "grabbed" : "free"}`, text: d.managed ? "grabbed" : "free" }),
        el("span", { class: "model", text: d.model }),
        el("span", { class: "muted", text: d.name }),
        el("span", { class: "path", text: d.path }),
        el("span", { class: "path", text: "0x" + d.product_id.toString(16).padStart(4, "0") }),
      )
    );
  }
}

function renderHardware() {
  const h = server.hardware;
  const rows = [
    ["config node", h.config_node || "—"],
    ["hidraw readable", String(h.hidraw_readable)],
    ["usbfs node", h.usbfs_node || "—"],
    ["usbfs writable", String(h.usbfs_writable)],
    ["opcode table", h.protocol_recovered ? "recovered" : "not recovered"],
  ];
  const grid = el("dl", { class: "hw-grid" });
  for (const [k, v] of rows) {
    grid.append(el("dt", { text: k }), el("dd", { text: v }));
  }
  $("hardware").replaceChildren(grid, el("p", { class: "hw-msg", text: h.message }));
}

function renderProfileForm() {
  if (!draft) return;
  $("name-input").value = draft.name;
  $("pointer-speed").value = draft.pointer_speed;
  $("scroll-speed").value = draft.scroll_speed;
  $("pointer-val").textContent = Number(draft.pointer_speed).toFixed(2) + "×";
  $("scroll-val").textContent = Number(draft.scroll_speed).toFixed(2) + "×";
  $("inv-x").checked = !!draft.invert_pointer_x;
  $("inv-y").checked = !!draft.invert_pointer_y;
  $("inv-scroll").checked = !!draft.invert_scroll;
}

function renderProfileSelect() {
  const sel = $("profile-select");
  sel.replaceChildren();
  for (const p of server.profiles) {
    sel.append(el("option", { value: p.name, selected: p.name === (draft && draft.name), text: p.name }));
  }
  if (draft && !server.profiles.some((p) => p.name === draft.name)) {
    sel.append(el("option", { value: draft.name, selected: true, text: draft.name + " (unsaved)" }));
  }
}

const ACTION_TYPES = [
  ["default", "Do nothing extra"],
  ["none", "Disable"],
  ["button", "Other button"],
  ["key", "Keyboard key"],
  ["macro", "Run macro"],
];

function actionType(action) {
  return action && action.type ? action.type : "default";
}

function renderMappings() {
  const box = $("mappings");
  box.replaceChildren();
  const buttons = draft.buttons || {};
  const entries = Object.entries(buttons);
  if (!entries.length) {
    box.append(el("p", { class: "empty", text: "No custom mappings — every button behaves as shipped." }));
    return;
  }
  for (const [source, action] of entries) box.append(mappingRow(source, action));
}

function mappingRow(source, action) {
  const type = actionType(action);

  const src = el("select", {
    class: "grew",
    onchange: (e) => {
      const to = e.target.value;
      if (to === source) return;
      if (buttonsHas(to)) {
        e.target.value = source;
        toast(`${btnLabel(to)} is already mapped — edit that row instead`, "error");
        return;
      }
      const b = draft.buttons;
      b[to] = b[source];
      delete b[source];
      renderMappings();
    },
    children: [option(source, btnLabel(source))],
  });
  for (const b of btnKeys) if (b.code !== source) src.append(option(b.code, b.label));

  const act = el("select", {
    onchange: (e) => {
      draft.buttons[source] = blankAction(e.target.value);
      renderMappings();
    },
    children: ACTION_TYPES.map(([v, label]) => option(v, label, v === type)),
  });

  const row = el("div", { class: "mapping", "data-source": source }, src, act);

  if (type === "button") {
    const target = el("select", {
      class: "grew",
      onchange: (e) => { action.code = e.target.value; },
      children: btnKeys.map((b) => option(b.code, b.label, b.code === action.code)),
    });
    if (action.code && !btnKeys.some((b) => b.code === action.code)) {
      target.prepend(option(action.code, btnLabel(action.code), true));
    }
    row.append(target);
  }

  if (type === "key") {
    const target = el("input", {
      class: "grew",
      type: "text",
      list: "all-keys",
      value: action.code || "",
      placeholder: "KEY_C",
      onchange: (e) => { action.code = e.target.value.toUpperCase(); },
    });
    row.append(target);

    const mods = el("div", { class: "mods" }, el("span", { class: "lbl", text: "Hold:" }));
    const held = new Set(action.modifiers || []);
    for (const m of ["KEY_LEFTCTRL", "KEY_LEFTSHIFT", "KEY_LEFTALT", "KEY_LEFTMETA"]) {
      mods.append(
        el("label", { class: "check" },
          el("input", {
            type: "checkbox",
            checked: held.has(m),
            onchange: (e) => {
              const set = new Set(action.modifiers || []);
              e.target.checked ? set.add(m) : set.delete(m);
              action.modifiers = [...set];
            },
          }),
          m.replace("KEY_LEFT", "").replace("KEY_", ""),
        )
      );
    }
    row.append(mods);
  }

  if (type === "macro") {
    const opts = Object.values(draft.macros || {});
    if (!opts.length) opts.push({ id: "", name: "(no macros defined)" });
    row.append(
      el("select", {
        class: "grew",
        onchange: (e) => { action.id = e.target.value; },
        children: opts.map((m) => option(m.id, m.name, m.id === action.id)),
      })
    );
  }

  row.append(
    el("button", {
      class: "btn sm danger",
      type: "button",
      text: "Remove",
      onclick: () => { delete draft.buttons[source]; renderMappings(); },
    })
  );
  return row;
}

function buttonsHas(name) {
  return Object.prototype.hasOwnProperty.call(draft.buttons || {}, name);
}

function blankAction(type) {
  if (type === "button") return { type, code: "BTN_RIGHT" };
  if (type === "key") return { type, code: "KEY_C", modifiers: [] };
  if (type === "macro") return { type, id: Object.keys(draft.macros || {})[0] || "" };
  return { type };
}

function option(value, label, selected) {
  return el("option", { value, text: label, selected: !!selected });
}

// ------------------------------------------------------------------- macros

function renderMacros() {
  const box = $("macros");
  box.replaceChildren();
  const macros = Object.values(draft.macros || {});
  if (!macros.length) {
    box.append(el("p", { class: "empty", text: "No macros defined." }));
    return;
  }
  for (const m of macros) box.append(macroCard(m));
}

function macroCard(m) {
  const card = el("div", { class: "macro" });
  card.append(
    el("input", {
      class: "grew",
      type: "text",
      value: m.name,
      placeholder: "Macro name",
      oninput: (e) => { m.name = e.target.value; },
    }),
    el("button", {
      class: "btn sm danger",
      type: "button",
      text: "Delete macro",
      onclick: () => { delete draft.macros[m.id]; renderMacros(); renderMappings(); },
    }),
  );

  const steps = el("div", { class: "steps" });
  (m.steps || []).forEach((step, i) => {
    const row = el("div", { class: "step" },
      el("span", { class: "idx", text: String(i + 1) }),
      el("input", {
        class: "grew",
        type: "text",
        list: "all-keys",
        value: step.code,
        placeholder: "KEY_A",
        oninput: (e) => { step.code = e.target.value.toUpperCase(); },
      }),
      el("select", {
        title: "tap = press+release, down/up = hold across steps",
        onchange: (e) => { step.action = e.target.value; },
        children: ["tap", "down", "up"].map((a) =>
          option(a, a, (step.action || "tap") === a)
        ),
      }),
      el("input", {
        type: "number",
        min: "0",
        max: "10000",
        step: "50",
        value: String(step.delay_ms || 0),
        title: "Delay after this key (ms)",
        style: "width:92px",
        oninput: (e) => { step.delay_ms = Math.max(0, Number(e.target.value) || 0); },
      }),
      el("span", { class: "lbl muted", text: "ms" }),
      el("button", {
        class: "btn sm",
        type: "button",
        text: "×",
        onclick: () => { m.steps.splice(i, 1); renderMacros(); },
      }),
    );
    steps.append(row);
  });
  steps.append(
    el("button", {
      class: "btn sm",
      type: "button",
      text: "+ Add step",
      style: "align-self:flex-start",
      onclick: () => { (m.steps ||= []).push({ code: "KEY_A", delay_ms: 0 }); renderMacros(); },
    })
  );
  card.append(steps);
  if ((m.steps || []).some((s) => (s.action || "tap") === "tap" && MODIFIER_CODES.has(s.code))) {
    card.append(el("p", {
      class: "hint",
      text: "A modifier set to tap is released before the next key. For shortcuts, set it to down and add a final up step.",
    }));
  }
  return card;
}

// -------------------------------------------------------------------- save

function readFormIntoDraft() {
  if (!draft) return;
  const name = $("name-input").value.trim();
  if (name) draft.name = name;
  draft.pointer_speed = Number($("pointer-speed").value);
  draft.scroll_speed = Number($("scroll-speed").value);
  draft.invert_pointer_x = $("inv-x").checked;
  draft.invert_pointer_y = $("inv-y").checked;
  draft.invert_scroll = $("inv-scroll").checked;
}

async function save() {
  if (recording) {
    toast("Stop recording first", "error");
    throw new Error("recording in progress");
  }
  readFormIntoDraft();
  await api("/api/profiles/" + encodeURIComponent(draft.name), "PUT", draft);
  if (originalName && originalName !== draft.name) {
    try {
      await api("/api/profiles/" + encodeURIComponent(originalName), "DELETE");
    } catch (_) { /* it may never have existed on disk */ }
  }
  originalName = draft.name;
  await poll();
  renderProfileSelect();
}

async function activate() {
  await save();
  await api("/api/profiles/" + encodeURIComponent(draft.name) + "/activate", "POST", {});
  toast(`Saved and activated “${draft.name}”`);
  await poll();
}

function loadProfile(name) {
  if (recording) stopRecording(false);
  const found = server.profiles.find((p) => p.name === name);
  if (!found) return;
  draft = clone(found);
  originalName = draft.name;
  renderProfileForm();
  renderMappings();
  renderMacros();
  renderProfileSelect();
}

// ----------------------------------------------------------------- recorder

function startRecording() {
  if (recording) return;
  if (!draft) { toast("Load a profile first", "error"); return; }
  recording = { steps: [], last: performance.now() };
  $("rec-overlay").hidden = false;
  $("recorder").hidden = false;
  renderRecSteps();
  if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
}

function renderRecSteps() {
  const steps = recording ? recording.steps : [];
  $("rec-count").textContent = `${steps.length} step${steps.length === 1 ? "" : "s"}`;
  $("rec-steps").replaceChildren(
    ...steps.map((s, i) =>
      el("span", { class: "rec-step" },
        el("b", { text: String(i + 1) }),
        ` ${s.code} ${s.action}`,
      )
    )
  );
}

function addRecStep(code, action) {
  if (!recording) return;
  const now = performance.now();
  const gap = Math.max(0, Math.min(Math.round(now - recording.last), 30000));
  if (recording.steps.length) {
    recording.steps[recording.steps.length - 1].delay_ms = gap;
  }
  recording.last = now;
  recording.steps.push({ code, action, delay_ms: 0 });
  renderRecSteps();
  if (recording.steps.length >= 300) {
    stopRecording(true);
    toast("Macro capped at 300 steps", "error");
  }
}

function stopRecording(save) {
  const rec = recording;
  recording = null;
  $("rec-overlay").hidden = true;
  $("recorder").hidden = true;
  if (!rec || !save) return;
  if (!rec.steps.length) {
    toast("Nothing recorded — press a key or click first", "error");
    return;
  }
  const name = `Macro ${Object.keys(draft.macros || {}).length + 1}`;
  const id = "m" + Date.now().toString(36);
  draft.macros ||= {};
  draft.macros[id] = { id, name, steps: rec.steps };
  renderMacros();
  renderMappings();
  toast(`Recorded “${name}” — click Save & activate to keep it`, "ok");
}

function isRecorderField(target) {
  return target instanceof Element && target.closest("#recorder") &&
    /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName);
}

function recordKey(e, action) {
  if (!recording || isRecorderField(e.target)) return;
  const code = CODE_TO_EVDEV[e.code];
  if (!code) { e.preventDefault(); return; }
  e.preventDefault();
  e.stopPropagation();
  if (action === "down" && e.repeat) return;
  addRecStep(code, action);
}

function recordMouse(e, action) {
  if (!recording) return;
  if (e.target.closest && e.target.closest("#recorder")) return;
  const code = MOUSE_BUTTONS[e.button];
  if (!code) return;
  e.preventDefault();
  e.stopPropagation();
  addRecStep(code, action);
}

// --------------------------------------------------------------------- boot

function wire() {
  $("refresh-btn").onclick = async () => { await poll(); toast("Refreshed"); };

  $("profile-select").onchange = (e) => loadProfile(e.target.value);

  $("name-input").oninput = (e) => { draft.name = e.target.value; };
  for (const id of ["pointer-speed", "scroll-speed"]) {
    $(id).oninput = () => {
      readFormIntoDraft();
      $("pointer-val").textContent = Number(draft.pointer_speed).toFixed(2) + "×";
      $("scroll-val").textContent = Number(draft.scroll_speed).toFixed(2) + "×";
    };
  }
  for (const id of ["inv-x", "inv-y", "inv-scroll"]) {
    $(id).onchange = readFormIntoDraft;
  }

  $("save-btn").onclick = async () => {
    try { await save(); toast(`Saved “${draft.name}”`); }
    catch (e) { toast(e.message, "error"); }
  };
  $("save-activate-btn").onclick = async () => {
    try { await activate(); }
    catch (e) { toast(e.message, "error"); }
  };

  $("delete-btn").onclick = async () => {
    readFormIntoDraft();
    if (!originalName) { toast("Profile is not saved yet", "error"); return; }
    if (!confirm(`Delete profile “${originalName}”?`)) return;
    try {
      await api("/api/profiles/" + encodeURIComponent(originalName), "DELETE");
      await poll();
      loadProfile(server.active_name);
      toast(`Deleted “${originalName}”`);
    } catch (e) { toast(e.message, "error"); }
  };

  $("new-btn").onclick = () => {
    if (recording) stopRecording(false);
    const name = (prompt("Name for the new profile:", `Profile ${server.profiles.length + 1}`) || "").trim();
    if (!name) return;
    draft = {
      name,
      pointer_speed: 1.0,
      scroll_speed: 1.0,
      invert_pointer_x: false,
      invert_pointer_y: false,
      invert_scroll: false,
      buttons: {},
      macros: {},
    };
    originalName = null;
    renderProfileForm();
    renderProfileSelect();
    renderMappings();
    renderMacros();
  };

  $("add-mapping-btn").onclick = () => {
    readFormIntoDraft();
    draft.buttons ||= {};
    const used = new Set(Object.keys(draft.buttons));
    const free = btnKeys.find((b) => !used.has(b.code));
    if (!free) { toast("Every device button is already mapped", "error"); return; }
    draft.buttons[free.code] = { type: "key", code: "KEY_C", modifiers: [] };
    renderMappings();
  };

  $("add-macro-btn").onclick = () => {
    readFormIntoDraft();
    draft.macros ||= {};
    const id = "m" + (Date.now().toString(36));
    draft.macros[id] = { id, name: `Macro ${Object.keys(draft.macros).length + 1}`, steps: [{ code: "KEY_A", delay_ms: 0 }] };
    renderMacros();
  };

  $("record-macro-btn").onclick = startRecording;
  $("rec-stop").onclick = () => stopRecording(true);
  $("rec-cancel").onclick = () => stopRecording(false);

  window.addEventListener("keydown", (e) => recordKey(e, "down"), true);
  window.addEventListener("keyup", (e) => recordKey(e, "up"), true);
  window.addEventListener("mousedown", (e) => recordMouse(e, "down"), true);
  window.addEventListener("mouseup", (e) => recordMouse(e, "up"), true);
  window.addEventListener("contextmenu", (e) => { if (recording) e.preventDefault(); }, true);
}

async function boot() {
  let deviceButtons = [];
  try {
    [keys, deviceButtons] = await Promise.all([api("/api/keys"), api("/api/buttons")]);
  } catch (e) {
    toast(e.message, "error");
    return;
  }
  // Only offer buttons the connected trackball actually produces. Fall back to
  // the generic list if the endpoint is unavailable (e.g. old daemon).
  const rawButtons = deviceButtons.length
    ? deviceButtons
    : keys.filter((k) => k.startsWith("BTN_")).map((code) => ({ code, label: code }));
  btnKeys = rawButtons
    .map((b) => (typeof b === "string" ? { code: b, label: b } : b))
    .sort((a, b) => a.code.localeCompare(b.code));
  btnLabels = Object.fromEntries(btnKeys.map((b) => [b.code, b.label]));
  keyKeys = keys.filter((k) => k.startsWith("KEY_")).sort();
  const dl = $("all-keys");
  dl.replaceChildren(...keys.map((k) => el("option", { value: k })));

  wire();
  await poll();
  loadProfile(server.active_name);
  renderProfileForm();

  setInterval(async () => {
    try { await poll(); } catch (_) { /* daemon may be restarting */ }
  }, 4000);
}

boot();
