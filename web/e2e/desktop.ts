// End-to-end test of the desktop app's frontend and Rust side, in headless Firefox: the debug build of
// `memo-desktop --serve` serves the web UI and answers its IPC calls over HTTP (`?ipc=http`), with
// secrets in a file instead of the keyring. Only the Tauri window is left out.
//
//   bun run build && cargo build -p memo-desktop && cargo build --release -p memo-server
//   bun run e2e:desktop
//
// MEMO_SERVER and MEMO_DESKTOP override the binaries, E2E_KEEP=1 keeps the temporary directory.
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Browser, Tab, sleep } from "./bidi";

const SERVER = process.env.MEMO_SERVER ?? resolve(import.meta.dir, "../../target/release/memo-server");
const DESKTOP = process.env.MEMO_DESKTOP ?? resolve(import.meta.dir, "../../target/debug/memo-desktop");
const DIST = resolve(import.meta.dir, "../dist");
const PORT = Number(process.env.E2E_PORT ?? 18082);
const APP_PORT = Number(process.env.E2E_APP_PORT ?? 18083);
const BIDI_PORT = Number(process.env.E2E_BIDI_PORT ?? 9334);
const SERVER_URL = `http://127.0.0.1:${PORT}`;
const APP = `http://127.0.0.1:${APP_PORT}/?ipc=http`;
const PW = "correct horse battery staple";

const dir = mkdtempSync(join(tmpdir(), "memo-e2e-desktop-"));
const config = join(dir, "config.toml");
writeFileSync(config, `listen = "127.0.0.1:${PORT}"\npublic_url = "${SERVER_URL}"\ndata_dir = "${join(dir, "data")}"\n`);
const invite = execFileSync(SERVER, ["--config", config, "invite", "create"], {
  encoding: "utf8",
  stdio: ["ignore", "pipe", "ignore"],
}).trim();

const log = join(dir, "processes.log");
writeFileSync(log, "");
function start(binary: string, args: string[]): ChildProcess {
  const proc = spawn(binary, args, { stdio: ["ignore", "ignore", "pipe"] });
  proc.stderr!.on("data", (chunk) => writeFileSync(log, chunk, { flag: "a" }));
  return proc;
}
async function up(url: string, what: string) {
  for (let i = 0; ; i++) {
    if (await fetch(url).then(() => true, () => false)) return;
    if (i === 100) throw new Error(`${what} did not start, see ${log}`);
    await sleep(100);
  }
}
const startServer = () => start(SERVER, ["--config", config, "serve"]);
const startDesktop = () => start(DESKTOP, ["--serve", String(APP_PORT), join(dir, "desktop"), DIST]);
async function stop(proc: ChildProcess) {
  proc.kill();
  await new Promise((done) => (proc.exitCode !== null ? done(null) : proc.once("exit", done)));
}

let server = startServer();
let desktop = startDesktop();
const browser = new Browser();
let step = "start";
let failed = false;

function check(ok: unknown, what: string) {
  if (!ok) throw new Error(`check failed: ${what}`);
  console.log(`  ok  ${what}`);
}
function section(name: string) {
  step = name;
  console.log(name);
}
async function open(tab: Tab, title: string) {
  await tab.until(`[...document.querySelectorAll(".item")].some((i) => i.innerText.includes(${JSON.stringify(title)}))`, `"${title}" in the list`, 20000);
  await tab.eval(`[...document.querySelectorAll(".item")].find((i) => i.innerText.includes(${JSON.stringify(title)})).click()`);
  await tab.until(`document.querySelector(".pane .toolbar")`, "memo pane");
}
async function write(tab: Tab, body: string) {
  await tab.until(`document.querySelector("textarea.editor")`, "editor");
  await tab.fill("textarea.editor", body);
  await sleep(300);
  await tab.button("Done");
  await tab.until(`document.querySelector(".markdown h1")`, "rendered memo");
}
const status = (tab: Tab) => tab.text(".status .text");
const synced = (tab: Tab) => tab.until(`document.querySelector(".status")?.innerText.startsWith("Synced")`, "synced", 20000);

try {
  await up(SERVER_URL, "memo-server");
  await up(APP, "memo-desktop --serve");
  await browser.start(dir, BIDI_PORT);

  section("register on the desktop");
  const app = await browser.newTab();
  await app.go(`${APP}#/invite/${invite}`);
  await app.until(`document.querySelector('input[autocomplete="url"]')`, "server field");
  await app.fill('input[autocomplete="url"]', SERVER_URL);
  await app.fill('input[autocomplete="username"]', "alice");
  await app.fill("form input[type=password]", PW);
  await app.click("form button.primary");
  await app.until(`document.querySelector(".key")`, "recovery key", 30000);
  const groups = (await app.text(".key"))!.trim().split(/\s+/);
  await app.fill("form input.mono", groups[groups.length - 1]);
  await app.click("form button.primary");
  await app.until(`document.querySelector(".sidebar")`, "main view");
  check(true, "registered against the server");

  await app.button("New memo");
  await write(app, "# Desktop memo\n\nWritten on the desktop. [link](https://example.org)");
  await synced(app);
  check((await app.text(".markdown h1")) === "Desktop memo", "memo rendered by the Rust side");

  section("image");
  await app.button("Edit", ".toolbar");
  await app.until(`document.querySelector("textarea.editor")`, "editor");
  await app.eval(`(async () => {
    const c = new OffscreenCanvas(1200, 800); const g = c.getContext("2d");
    g.fillStyle = "#37a"; g.fillRect(0, 0, 1200, 800);
    const blob = await c.convertToBlob({ type: "image/png" });
    const dt = new DataTransfer(); dt.items.add(new File([blob], "test.png", { type: "image/png" }));
    const input = document.querySelector("input[type=file]"); input.files = dt.files; input.dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
  await app.until(`document.querySelector("textarea.editor").value.includes("attachment:")`, "image inserted", 20000);
  await sleep(300);
  await app.button("Done");
  await app.until(`document.querySelector(".markdown img")?.complete && document.querySelector(".markdown img").naturalWidth > 0`, "image shown", 20000);
  await synced(app);
  check(true, "image cleaned, attached and uploaded");

  section("offline");
  await stop(server);
  await app.button("Edit", ".toolbar");
  await write(app, "# Desktop memo\n\nEdited offline.");
  await app.until(`document.querySelector(".status")?.innerText.includes("not synced") || document.querySelector(".status")?.innerText.includes("Not synced")`, "unsynced", 10000);
  check(true, `edit saved locally while the server is down ("${await status(app)}")`);

  section("restart");
  await stop(desktop);
  desktop = startDesktop();
  await up(APP, "memo-desktop --serve");
  await app.reload();
  await app.until(`document.querySelector(".sidebar")`, "main view without a password", 20000);
  await open(app, "Desktop memo");
  check((await app.text(".markdown"))!.includes("Edited offline."), "the offline edit survived the restart, keys from the store");

  section("sync when the server is back");
  server = startServer();
  await up(SERVER_URL, "memo-server");
  await app.button("Sync");
  await synced(app);
  check(true, "synced");

  section("the web client sees it");
  const web = await browser.newTab(await browser.userContext());
  await web.go(SERVER_URL);
  await web.until(`document.querySelector('input[autocomplete="username"]')`, "sign-in");
  check(!(await web.eval(`!!document.querySelector('input[autocomplete="url"]')`)), "the browser asks for no server");
  await web.fill('input[autocomplete="username"]', "alice");
  await web.fill("input[type=password]", PW);
  await web.click("form button.primary");
  await web.until(`document.querySelector(".sidebar")`, "main view", 30000);
  await open(web, "Desktop memo");
  check((await web.text(".markdown"))!.includes("Edited offline."), "the offline edit reached the server");

  await web.button("Edit", ".toolbar");
  await write(web, "# Desktop memo\n\nEdited on the web.");
  await synced(web);
  await app.button("Sync");
  await synced(app);
  await app.until(`document.querySelector(".markdown")?.innerText.includes("Edited on the web.")`, "web edit on the desktop", 10000);
  check(true, "the web edit reached the desktop");

  section("highlighting, links, filters and stats");
  await app.button("New memo");
  await write(app, "# Snippet\n\n```rust\nfn main() {}\n```\n\nSee ");
  check((await app.text(".markdown .hl-keyword")) === "fn", "code highlighted by the Rust side");
  await app.button("Edit", ".toolbar");
  await app.until(`document.querySelector("textarea.editor")`, "editor");
  await app.button("Link memo");
  await app.until(`[...document.querySelectorAll("dialog .results button")].some((b) => b.innerText.includes("Desktop memo"))`, "memo picker");
  await app.eval(`[...document.querySelectorAll("dialog .results button")].find((b) => b.innerText.includes("Desktop memo")).click()`);
  await app.until(`document.querySelector("textarea.editor").value.includes("(memo:")`, "link inserted");
  await sleep(300);
  await app.button("Done");
  await app.until(`document.querySelector(".markdown a.memo-link")`, "memo link");
  await app.click(".markdown a.memo-link");
  await app.until(`document.querySelector(".backlinks")?.innerText.includes("Snippet")`, "backlink", 10000);
  check(true, "a memo link opens its memo, which lists the backlink");
  await app.button("Code", ".quick");
  await app.until(`document.querySelectorAll(".item").length === 1`, "quick filter");
  await app.button("Save", ".filter");
  await app.until(`document.querySelector("dialog input[type=text]")`, "save dialog");
  await app.fill("dialog input[type=text]", "With code");
  await app.click("dialog form button.primary");
  await app.until(`document.querySelector(".saved")?.innerText.includes("With code")`, "saved filter");
  await app.button("Clear", ".filter");
  await app.click(".calendar .day.today");
  await app.until(`document.querySelectorAll(".item").length === 2`, "calendar day");
  await app.button("Clear", ".filter");
  await synced(app);
  await web.button("Sync");
  await web.until(`document.querySelector(".saved")?.innerText.includes("With code")`, "saved filter on the web", 20000);
  check(true, "quick filter saved, synced to the web client; calendar day picked");
  await app.go(`${APP}#/settings`);
  await app.until(`document.querySelector(".stats")`, "statistics");
  check((await app.text(".stats"))!.includes("Memos\n2"), "statistics from the Rust side");

  section("lock and unlock");
  await app.go(`${APP}#/settings`);
  await app.until(`document.querySelector(".devices li")`, "devices");
  check((await app.text(".devices"))!.includes("Desktop"), "listed as a desktop device");
  await app.button("Lock");
  await app.until(`document.querySelector("h1")?.innerText === "Unlock"`, "unlock screen");
  check(!existsSync(join(dir, "desktop", "secrets.json")), "lock removes the stored keys");
  await app.fill("input[type=password]", PW);
  await app.click("form button.primary");
  await app.until(`document.querySelector(".sidebar")`, "unlocked", 30000);
  await app.go(`${APP}#/`);
  await open(app, "Desktop memo");
  check(true, "unlocked; the memos stayed");

  section("sign out");
  await app.go(`${APP}#/settings`);
  await app.until(`document.querySelector(".devices li")`, "devices");
  await app.button("Sign out", "section:has(> .row)");
  await app.until(`document.querySelector("h1")?.innerText === "Sign in"`, "sign-in");
  const files = readdirSync(join(dir, "desktop", "data", "db"));
  check(!files.some((f) => f.endsWith(".db")), `the local copy is deleted (${files.join(", ") || "empty"})`);
  check(!existsSync(join(dir, "desktop", "data", "account.json")), "the account file is deleted");

  section("console");
  const errors = browser.logs.filter((l) => l.level === "error" || /Content-Security-Policy|CSP/.test(l.text));
  for (const e of errors) console.log(`  [${e.level}] ${e.text}`);
  check(errors.length === 0, "no errors or CSP violations in the console");
} catch (e) {
  failed = true;
  console.error(`\nFAILED in "${step}": ${e instanceof Error ? e.message : e}`);
  for (const l of browser.logs) console.error(`  console [${l.level}] ${l.text}`);
  console.error(`log:\n${readFileSync(log, "utf8").split("\n").slice(-30).join("\n")}`);
} finally {
  browser.stop();
  server.kill();
  desktop.kill();
  await sleep(200);
  if (process.env.E2E_KEEP || failed) console.log(`kept ${dir}`);
  else rmSync(dir, { recursive: true, force: true });
}
console.log(failed ? "FAILED" : "all passed");
process.exit(failed ? 1 : 0);
