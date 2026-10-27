// End-to-end test of the web app in headless Firefox against a real memo-server.
//
//   cargo build --release -p memo-server   (after `bun run build`, so the app is embedded)
//   bun run e2e
//
// MEMO_SERVER overrides the binary (default ../target/release/memo-server), E2E_SHOTS=<dir> saves
// screenshots, E2E_KEEP=1 keeps the temporary directory (server data, log, profile, downloads).
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { Browser, Tab, sleep } from "./bidi";

const SERVER = process.env.MEMO_SERVER ?? resolve(import.meta.dir, "../../target/release/memo-server");
const PORT = Number(process.env.E2E_PORT ?? 18081);
const BIDI_PORT = Number(process.env.E2E_BIDI_PORT ?? 9333);
const SHOTS = process.env.E2E_SHOTS;
const URL = `http://127.0.0.1:${PORT}`;
const PW = "correct horse battery staple";
const PW2 = "an even better passphrase";

const dir = mkdtempSync(join(tmpdir(), "memo-e2e-"));
const config = join(dir, "config.toml");
writeFileSync(config, `listen = "127.0.0.1:${PORT}"\npublic_url = "${URL}"\ndata_dir = "${join(dir, "data")}"\n`);

const invite = () => execFileSync(SERVER, ["--config", config, "invite", "create"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
const invites = { alice: invite(), bob: invite(), carol: invite() };

const log = join(dir, "server.log");
writeFileSync(log, "");
const server = spawn(SERVER, ["--config", config, "serve"], { stdio: ["ignore", "ignore", "pipe"] });
server.stderr!.on("data", (chunk) => writeFileSync(log, chunk, { flag: "a" }));

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
async function shot(tab: Tab, name: string) {
  if (SHOTS) await tab.shot(join(SHOTS, `${name}.png`));
}

async function register(tab: Tab, name: keyof typeof invites, password = PW): Promise<string> {
  await tab.go(`${URL}/#/invite/${invites[name]}`);
  await tab.until(`document.querySelector('input[autocomplete="username"]')`, "register form");
  await tab.fill('input[autocomplete="username"]', name);
  await tab.fill("form input[type=password]", password);
  await tab.click("form button.primary");
  return confirmKey(tab);
}
/** Confirms the recovery key screen and returns the key. */
async function confirmKey(tab: Tab): Promise<string> {
  await tab.until(`document.querySelector(".key")`, "recovery key", 30000);
  const groups = (await tab.text(".key"))!.trim().split(/\s+/);
  await tab.fill("form input.mono", groups[groups.length - 1]);
  await tab.click("form button.primary");
  await tab.until(`document.querySelector(".sidebar")`, "main view");
  return groups.join("-");
}
/** Signs in, or unlocks when the session survived a reload. */
async function login(tab: Tab, name: string, password = PW) {
  await tab.go(URL);
  await tab.until(`document.querySelector('input[autocomplete="username"]:not([hidden])') || document.querySelector("h1")?.innerText === "Unlock"`, "sign-in or unlock");
  if (await tab.eval(`!!document.querySelector('input[autocomplete="username"]:not([hidden])')`)) {
    await tab.fill('input[autocomplete="username"]', name);
  }
  await tab.fill("input[type=password]", password);
  await tab.click("form button.primary");
  await tab.until(`document.querySelector(".sidebar")`, "main view", 30000);
}
async function open(tab: Tab, title: string) {
  await tab.until(`[...document.querySelectorAll(".item")].some((i) => i.innerText.includes(${JSON.stringify(title)}))`, `"${title}" in the list`, 20000);
  await tab.eval(`[...document.querySelectorAll(".item")].find((i) => i.innerText.includes(${JSON.stringify(title)})).click()`);
  await tab.until(`document.querySelector(".pane .toolbar")`, "memo pane");
}
const synced = (tab: Tab) => tab.until(`document.querySelector(".status")?.innerText.startsWith("Synced")`, "synced", 20000);
const title = (tab: Tab) => tab.text(".markdown h1");

async function share(alice: Tab, name: string) {
  await alice.button("Share", ".toolbar");
  await alice.until(`document.querySelector("dialog[open] .members li")`, "share dialog");
  const before = await alice.count("dialog[open] .members li");
  await alice.fill("dialog[open] form input", name);
  await alice.click("dialog[open] form button");
  await alice.until(`document.querySelector("dialog[open] .found")`, `${name} found`);
  await alice.eval(`(() => { const s = document.querySelector("dialog[open] .found select"); s.value = "write"; s.dispatchEvent(new Event("change", { bubbles: true })); })()`);
  await alice.button("Share", "dialog[open] .found");
  await alice.until(`document.querySelectorAll("dialog[open] .members li").length === ${before + 1}`, `${name} is a member`, 20000);
}

try {
  for (let i = 0; ; i++) {
    const page = await fetch(URL).then((r) => r.text()).catch(() => null);
    if (page !== null) {
      if (!page.includes("<script")) throw new Error("memo-server was built without the web app: run `bun run build` first");
      break;
    }
    if (i === 100) throw new Error(`memo-server did not start, see ${log}`);
    await sleep(100);
  }
  await browser.start(dir, BIDI_PORT);

  section("register and write");
  const alice = await browser.newTab();
  await register(alice, "alice");
  await shot(alice, "01-empty");
  await alice.button("New memo");
  await alice.until(`document.querySelector("textarea.editor")`, "editor");
  await alice.fill("textarea.editor", "# Groceries\n\n- [ ] milk #shopping\n- [x] eggs\n\nSee **[the shop](https://example.org)**.\n<script>alert(1)</script>");
  await sleep(300);
  await alice.button("Done");
  await alice.until(`document.querySelector(".markdown h1")`, "rendered memo");
  await synced(alice);
  check((await title(alice)) === "Groceries", "heading rendered");
  check(await alice.eval(`!document.querySelector(".markdown script") && document.querySelector(".markdown").innerText.includes("<script>")`), "raw HTML is escaped");
  check(await alice.eval(`!!document.querySelector('.markdown a[href="https://example.org"]')`), "link rendered");
  check((await alice.text(".markdown .tag")) === "#shopping", "tag rendered");
  await shot(alice, "02-memo");

  await alice.click(".markdown input[data-task]");
  await alice.until(`document.querySelector(".markdown input[data-task]").checked`, "task toggled");
  await synced(alice);
  check(true, "task toggled from the rendered view");

  section("image");
  await alice.button("Edit", ".toolbar");
  await alice.until(`document.querySelector("textarea.editor")`, "editor");
  await alice.eval(`(async () => {
    const c = new OffscreenCanvas(1200, 800); const g = c.getContext("2d");
    g.fillStyle = "#3a7"; g.fillRect(0, 0, 1200, 800); g.fillStyle = "#fff"; g.font = "120px sans-serif"; g.fillText("memo", 300, 450);
    const blob = await c.convertToBlob({ type: "image/png" });
    const dt = new DataTransfer(); dt.items.add(new File([blob], "test.png", { type: "image/png" }));
    const input = document.querySelector("input[type=file]"); input.files = dt.files; input.dispatchEvent(new Event("change", { bubbles: true }));
  })()`);
  await alice.until(`document.querySelector("textarea.editor").value.includes("attachment:")`, "image reference inserted", 20000);
  await alice.button("Done");
  await alice.until(`document.querySelector(".markdown img.attachment")?.complete && document.querySelector(".markdown img.attachment").naturalWidth`, "image shown", 20000);
  await synced(alice);
  const [w, h] = await alice.eval(`(() => { const i = document.querySelector(".markdown img.attachment"); return [i.naturalWidth, i.naturalHeight]; })()`);
  check(w === 512 && h === 341, `memo shows the thumbnail (${w}×${h})`);
  await alice.click(".markdown img.attachment");
  await alice.until(`document.querySelector("dialog[open] img")?.complete && document.querySelector("dialog[open] img").naturalWidth`, "lightbox", 20000);
  const full = await alice.eval(`(() => { const i = document.querySelector("dialog[open] img"); return [i.naturalWidth, i.naturalHeight]; })()`);
  check(full[0] === 1200 && full[1] === 800, `lightbox shows the original (${full[0]}×${full[1]})`);
  await shot(alice, "03-image");
  await alice.eval(`document.querySelector("dialog[open]").close()`);

  section("reload and unlock");
  await alice.reload();
  await alice.until(`document.querySelector("h1")?.innerText === "Unlock"`, "unlock screen");
  await alice.fill("input[type=password]", PW);
  await alice.click("form button.primary");
  await alice.until(`document.querySelector(".item")`, "memos after unlock", 30000);
  await alice.button("Settings");
  await alice.until(`document.querySelector(".devices li")`, "devices");
  check((await alice.count(".devices li")) === 1, "unlocking replaced the device instead of adding one");
  check((await alice.eval(`document.querySelector(".fill").style.width`)).endsWith("%"), "style: directive works under the CSP");

  section("share");
  const bob = await browser.newTab(await browser.userContext());
  await register(bob, "bob");
  await open(alice, "Groceries");
  await share(alice, "bob");
  await shot(alice, "04-share");
  await alice.eval(`document.querySelector("dialog[open]").close()`);
  await bob.button("Sync");
  await open(bob, "Groceries");
  await bob.until(`document.querySelector(".markdown img.attachment")?.naturalWidth`, "bob sees the image", 20000);
  check((await title(bob)) === "Groceries", "bob can read the shared memo");
  check((await bob.text(".pane footer"))?.includes("alice"), "bob sees who owns it");

  section("conflict");
  for (const [tab, word] of [[alice, "Alice"], [bob, "Bob"]] as const) {
    await tab.button("Edit", ".toolbar");
    await tab.until(`document.querySelector("textarea.editor")`, "editor");
    await tab.eval(`(() => { const t = document.querySelector("textarea.editor"); t.value = t.value.replace("# Groceries", "# Groceries for ${word}"); t.dispatchEvent(new Event("input", { bubbles: true })); })()`);
  }
  await alice.button("Done");
  await synced(alice);
  await bob.button("Done");
  await bob.until(`document.querySelector(".conflict")`, "bob gets the conflict", 20000);
  check(!(await alice.eval(`!!document.querySelector(".conflict")`)), "only the later writer sees the conflict");
  await shot(bob, "05-conflict");
  await bob.click(".conflict section button.primary");
  await bob.until(`!document.querySelector(".conflict")`, "conflict resolved");
  await synced(bob);
  await alice.button("Sync");
  await alice.until(`document.querySelector(".markdown h1")?.innerText === ${JSON.stringify(await title(bob))}`, "both agree", 20000);
  check(true, `both show "${await title(alice)}"`);

  section("recovery");
  const carol = await browser.newTab(await browser.userContext());
  const key = await register(carol, "carol");
  await carol.button("New memo");
  await carol.until(`document.querySelector("textarea.editor")`, "editor");
  await carol.fill("textarea.editor", "Carol's secret plan");
  await sleep(300);
  await carol.button("Done");
  await synced(carol);
  await carol.button("Settings");
  await carol.button("Sign out", ".settings");
  await carol.until(`document.querySelector("h1")?.innerText === "Sign in"`, "signed out");
  await carol.button("Forgot password");
  await carol.until(`document.querySelector("input.mono")`, "recovery form");
  await carol.fill('input[autocomplete="username"]', "carol");
  await carol.fill("input.mono", key.toLowerCase());
  await carol.fill("form input[type=password]", PW2);
  await carol.click("form button.primary");
  check((await confirmKey(carol)) !== key, "recovery issues a new key");
  await open(carol, "Carol's secret plan");
  check(true, "memos survive recovery");

  section("history");
  await open(alice, "Groceries");
  await alice.button("History", ".toolbar");
  await alice.until(`document.querySelector("dialog[open] li")`, "revisions");
  check((await alice.count("dialog[open] li")) >= 3, "revisions listed");
  await alice.eval(`[...document.querySelectorAll("dialog[open] li button")].at(-1).click()`);
  await alice.until(`document.querySelector("dialog[open] .version .markdown")`, "old version");
  await alice.button("Restore this version", "dialog[open]");
  await alice.until(`!document.querySelector("dialog[open]")`, "dialog closed");
  await alice.until(`document.querySelector(".markdown h1")?.innerText === "Groceries"`, "restored title");
  await synced(alice);
  check(true, "first version restored");

  section("revoke and share again");
  await alice.eval(`window.confirm = () => true`);
  await alice.button("Share", ".toolbar");
  await alice.until(`document.querySelectorAll("dialog[open] .members li").length === 2`, "members");
  await alice.button("Remove", "dialog[open]");
  await alice.until(`document.querySelectorAll("dialog[open] .members li").length === 1`, "bob removed", 20000);
  await alice.eval(`document.querySelector("dialog[open]").close()`);
  await bob.button("Sync");
  await bob.until(`!document.querySelector(".item")`, "the memo leaves bob's list", 20000);
  check(true, "revoked memo is gone for bob");
  await share(alice, "bob");
  await alice.eval(`document.querySelector("dialog[open]").close()`);
  await bob.button("Sync");
  await open(bob, "Groceries");
  await bob.until(`document.querySelector(".markdown h1")?.innerText === "Groceries"`, "bob reads the new key", 20000);
  check(true, "bob can read it again after a re-share");

  section("archive and export");
  await alice.button("Archive", ".toolbar");
  await alice.until(`!document.querySelector(".item")`, "archived out of the list");
  await alice.button("Archive", ".sidebar");
  await alice.until(`document.querySelector(".item")`, "archive view");
  check((await alice.text(".items"))?.includes("Groceries"), "memo is in the archive");
  await alice.button("Settings");
  await alice.until(`document.querySelector(".settings form")`, "settings");
  await alice.eval(`(() => { window.blobs = []; const create = URL.createObjectURL; URL.createObjectURL = (b) => (window.blobs.push(b), create.call(URL, b)); })()`);
  await alice.button("Export memos");
  await alice.until(`window.blobs.some((b) => b.type === "application/zip")`, "export", 30000);
  const exported = await alice.eval(`(async () => {
    const bytes = new Uint8Array(await window.blobs.find((b) => b.type === "application/zip").arrayBuffer());
    const text = new TextDecoder().decode(bytes);
    return [bytes[0] === 0x50 && bytes[1] === 0x4b, text.includes("archive/Groceries"), text.includes("images/")];
  })()`);
  // The restored first version predates the image, so the image went with it.
  check(exported[0] && exported[1] && !exported[2], "export is a ZIP with the archived memo, without the dropped image");

  section("password change");
  const alice2 = await browser.newTab(await browser.userContext());
  await login(alice2, "alice");
  await alice.eval(`location.hash = "#/"`);
  await alice.button("Settings");
  await alice.until(`document.querySelectorAll(".devices li").length === 2`, "two devices");
  await alice.eval(`(() => {
    const [current, next, repeat] = document.querySelectorAll(".settings form")[0].querySelectorAll("input[type=password]");
    for (const [e, v] of [[current, ${JSON.stringify(PW)}], [next, ${JSON.stringify(PW2)}], [repeat, ${JSON.stringify(PW2)}]]) {
      e.value = v; e.dispatchEvent(new Event("input", { bubbles: true }));
    }
  })()`);
  await alice.button("Change password");
  await alice.until(`[...document.querySelectorAll(".toast")].some((t) => t.innerText.includes("Password changed"))`, "password changed", 30000);
  await alice.until(`document.querySelectorAll(".devices li").length === 1`, "other device signed out");
  check(true, "changing the password signs out the other devices");
  await alice2.button("Sync");
  await alice2.until(`document.querySelector("h1")?.innerText === "Sign in"`, "the other device is signed out", 20000);
  await alice.reload();
  await alice.until(`document.querySelector("h1")?.innerText === "Unlock"`, "unlock");
  await alice.fill("input[type=password]", PW2);
  await alice.click("form button.primary");
  await alice.until(`document.querySelector(".sidebar")`, "unlocked with the new password", 30000);
  check(true, "the new password unlocks");

  section("phone width");
  await alice.viewport(390, 780);
  await sleep(300);
  check((await alice.eval(`getComputedStyle(document.querySelector(".sidebar")).visibility`)) === "hidden", "menu is hidden");
  await alice.click("button.menu");
  await sleep(400);
  check((await alice.eval(`getComputedStyle(document.querySelector(".sidebar")).visibility`)) === "visible", "menu opens");
  await shot(alice, "06-phone-menu");

  section("console");
  const errors = browser.logs.filter((l) => l.level === "error" || /Content-Security-Policy|CSP/.test(l.text));
  for (const e of errors) console.log(`  [${e.level}] ${e.text}`);
  check(errors.length === 0, "no errors or CSP violations in the console");
} catch (e) {
  failed = true;
  console.error(`\nFAILED in "${step}": ${e instanceof Error ? e.message : e}`);
  for (const l of browser.logs) console.error(`  console [${l.level}] ${l.text}`);
  console.error(`server log:\n${readFileSync(log, "utf8").split("\n").slice(-30).join("\n")}`);
} finally {
  browser.stop();
  server.kill();
  await sleep(200);
  if (process.env.E2E_KEEP || failed) console.log(`kept ${dir}`);
  else rmSync(dir, { recursive: true, force: true });
}
console.log(failed ? "FAILED" : "all passed");
process.exit(failed ? 1 : 0);
