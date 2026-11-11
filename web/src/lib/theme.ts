// Light or dark: the user's choice on this device, or the system's. Sets `data-theme` on <html>,
// which app.css keys the dark palette on.

export type Theme = "system" | "light" | "dark";

const KEY = "memo-theme";
const system = window.matchMedia("(prefers-color-scheme: dark)");

export function theme(): Theme {
  const stored = localStorage.getItem(KEY);
  return stored === "light" || stored === "dark" ? stored : "system";
}

export function setTheme(choice: Theme) {
  if (choice === "system") localStorage.removeItem(KEY);
  else localStorage.setItem(KEY, choice);
  apply();
}

function apply() {
  const choice = theme();
  const dark = choice === "dark" || (choice === "system" && system.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

apply();
system.addEventListener("change", apply);
