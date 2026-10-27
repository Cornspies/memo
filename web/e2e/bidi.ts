// A minimal WebDriver BiDi client for headless Firefox: just what run.ts needs, so the tests
// need no browser-automation dependency.
import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export class Browser {
  ws!: WebSocket;
  next = 1;
  pending = new Map<number, (m: any) => void>();
  logs: { level: string; text: string }[] = [];
  proc?: ChildProcess;

  /** Starts Firefox with a fresh profile in `dir`. Downloads go to `dir/downloads`. */
  async start(dir: string, port: number) {
    const profile = `${dir}/profile`;
    mkdirSync(profile, { recursive: true });
    writeFileSync(
      `${profile}/user.js`,
      [
        'user_pref("browser.shell.checkDefaultBrowser", false);',
        'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
        'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);',
        'user_pref("browser.aboutwelcome.enabled", false);',
        'user_pref("browser.download.folderList", 2);',
        `user_pref("browser.download.dir", ${JSON.stringify(`${dir}/downloads`)});`,
        'user_pref("browser.download.useDownloadDir", true);',
      ].join("\n"),
    );
    this.proc = spawn(
      "firefox",
      ["--headless", "--no-remote", "--profile", profile, `--remote-debugging-port=${port}`, "about:blank"],
      { stdio: "ignore" },
    );
    for (let i = 0; ; i++) {
      try {
        this.ws = new WebSocket(`ws://127.0.0.1:${port}/session`);
        await new Promise((ok, err) => ((this.ws.onopen = ok), (this.ws.onerror = err)));
        break;
      } catch (e) {
        if (i === 100) throw new Error(`Firefox did not open its BiDi port ${port}`);
        await sleep(200);
      }
    }
    this.ws.onmessage = (e) => {
      const m = JSON.parse(String(e.data));
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)!(m);
        this.pending.delete(m.id);
      } else if (m.method === "log.entryAdded") {
        this.logs.push({ level: m.params.level, text: m.params.text });
      }
    };
    await this.send("session.new", { capabilities: {} });
    await this.send("session.subscribe", { events: ["log.entryAdded"] });
  }

  async send(method: string, params: any): Promise<any> {
    const id = this.next++;
    const reply = new Promise<any>((r) => this.pending.set(id, r));
    this.ws.send(JSON.stringify({ id, method, params }));
    const m = await reply;
    if (m.type === "error") throw new Error(`${method}: ${m.error}: ${m.message}`);
    return m.result;
  }

  async newTab(userContext?: string, width = 1280, height = 800): Promise<Tab> {
    const { context } = await this.send("browsingContext.create", { type: "window", ...(userContext ? { userContext } : {}) });
    const tab = new Tab(this, context);
    await tab.viewport(width, height);
    return tab;
  }

  /** A separate cookie jar: one per user. */
  async userContext(): Promise<string> {
    return (await this.send("browser.createUserContext", {})).userContext;
  }

  stop() {
    this.proc?.kill();
  }
}

export class Tab {
  constructor(
    public browser: Browser,
    public context: string,
  ) {}

  go(url: string) {
    return this.browser.send("browsingContext.navigate", { context: this.context, url, wait: "complete" });
  }
  reload() {
    return this.browser.send("browsingContext.reload", { context: this.context, wait: "complete" });
  }
  viewport(width: number, height: number) {
    return this.browser.send("browsingContext.setViewport", { context: this.context, viewport: { width, height } });
  }

  async eval(expression: string): Promise<any> {
    const r = await this.browser.send("script.evaluate", {
      expression,
      target: { context: this.context },
      awaitPromise: true,
      resultOwnership: "none",
    });
    if (r.type === "exception") throw new Error(`page exception: ${r.exceptionDetails.text}`);
    return unwrap(r.result);
  }

  async shot(path: string) {
    const { data } = await this.browser.send("browsingContext.captureScreenshot", { context: this.context });
    writeFileSync(path, Buffer.from(data, "base64"));
  }

  /** Waits until the expression is truthy. */
  async until(expression: string, what: string, ms = 15000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (await this.eval(`!!(${expression})`)) return;
      await sleep(150);
    }
    throw new Error(`timed out waiting for ${what}`);
  }

  text(selector: string): Promise<string | null> {
    return this.eval(`document.querySelector(${JSON.stringify(selector)})?.innerText ?? null`);
  }
  count(selector: string): Promise<number> {
    return this.eval(`document.querySelectorAll(${JSON.stringify(selector)}).length`);
  }

  /** Sets the value of every element matching `selector`, as if typed. */
  fill(selector: string, value: string) {
    return this.eval(`(() => {
      const all = document.querySelectorAll(${JSON.stringify(selector)});
      if (!all.length) throw new Error(${JSON.stringify(`no ${selector}`)});
      for (const e of all) { e.value = ${JSON.stringify(value)}; e.dispatchEvent(new Event("input", { bubbles: true })); }
    })()`);
  }

  click(selector: string) {
    return this.eval(`(() => {
      const e = document.querySelector(${JSON.stringify(selector)});
      if (!e) throw new Error(${JSON.stringify(`no ${selector}`)});
      e.click();
    })()`);
  }

  /** Clicks the first button inside `scope` (a selector) whose text is exactly `label`. */
  button(label: string, scope = "body") {
    return this.eval(`(() => {
      const b = [...document.querySelector(${JSON.stringify(scope)}).querySelectorAll("button")]
        .find((b) => b.innerText.trim() === ${JSON.stringify(label)});
      if (!b) throw new Error(${JSON.stringify(`no button "${label}" in ${scope}`)});
      b.click();
    })()`);
  }
}

function unwrap(v: any): any {
  switch (v?.type) {
    case "string":
    case "number":
    case "boolean":
      return v.value;
    case "undefined":
    case "null":
      return null;
    case "array":
      return v.value.map(unwrap);
    case "object":
      return Object.fromEntries(v.value.map(([k, x]: any) => [typeof k === "string" ? k : unwrap(k), unwrap(x)]));
    default:
      return v?.value ?? null;
  }
}
