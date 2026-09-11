const __invoke = (method, target, args = {}) => {
  const response = JSON.parse(__call(JSON.stringify({ method, target, args })));
  if (response.error) {
    const error = new Error(
      `[${response.error.code}] ${response.error.message}`,
    );
    error.code = response.error.code;
    throw error;
  }
  return response.value;
};
const __text = (value) =>
  typeof value === "string" ? value : JSON.stringify(value);
const __output = (value) => {
  if (!__emit(JSON.stringify(value)))
    throw new Error(
      "Output limit exceeded: emit at most 64 items and 16 MiB per call",
    );
};
const __observe = (value, options = {}) => {
  const text = __text(value);
  if (options.emit !== false) __output({ type: "text", text });
  return text;
};
const __image = (value, options = {}) => {
  if (options.emit !== false)
    __output({ type: "image", data: value.data, mimeType: value.mimeType });
  const bytes = Uint8Array.from(__decode(value.data));
  return bytes;
};
class Target {
  constructor(id) {
    this.id = id;
  }
  async getAXState(options = {}) {
    return __observe(__invoke("getAXState", this.id, options), options);
  }
  async getScreenshot(options = {}) {
    return __image(__invoke("getScreenshot", this.id), options);
  }
  async getAXStateAndScreenshot(options = {}) {
    const result = __invoke("getAXStateAndScreenshot", this.id, options);
    return {
      state: __observe(result.state, options),
      screenshot: __image(result.screenshot, options),
    };
  }
  async click(target, options = {}) {
    __invoke("click", this.id, { target, ...options });
  }
  async drag(from, to) {
    __invoke("drag", this.id, { from, to });
  }
  async pressKey(key) {
    __invoke("pressKey", this.id, { key });
  }
  async scroll(target, direction, pages = 1) {
    __invoke("scroll", this.id, { target, direction, pages });
  }
  async typeText(text) {
    __invoke("typeText", this.id, { text });
  }
  async paste(text, options = {}) {
    __invoke("paste", this.id, { text, ...options });
  }
  async setValue(elementIndex, value) {
    __invoke("setValue", this.id, { elementIndex, value });
  }
  async selectText(elementIndex, text, options = {}) {
    __invoke("selectText", this.id, { elementIndex, text, ...options });
  }
  async performSecondaryAction(elementIndex, action) {
    __invoke("performSecondaryAction", this.id, { elementIndex, action });
  }
}
globalThis.agentdesktop = Object.freeze({
  async getState(options = {}) {
    return __observe(__invoke("getState", null), options);
  },
  async listApps(options = {}) {
    return __observe(__invoke("listApps", null), options);
  },
  async getApp(query) {
    const result = __invoke("getApp", null, { query });
    __observe(result.state);
    return new Target(result.id);
  },
});
globalThis.nodeRepl = Object.freeze({
  write(value) {
    __observe(value);
  },
  async emitImage(bytes) {
    __output({
      type: "image",
      data: __encode(Array.from(bytes)),
      mimeType: "image/png",
    });
  },
});
