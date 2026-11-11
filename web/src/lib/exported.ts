// File names and links for the export (a ZIP of Markdown files), shared by both backends.

import type { MemoSummary } from "./types";

export function extension(mime: string) {
  return { "image/jpeg": "jpg", "image/png": "png", "image/webp": "webp", "image/gif": "gif" }[mime] ?? "bin";
}

function fileName(title: string, id: string) {
  const slug = title
    .replace(/[\\/:*?"<>|#\u0000-\u001f]/g, "")
    .trim()
    .slice(0, 60);
  return `${slug || "untitled"} (${id.slice(-8)})`;
}

/** Each memo's path in the export: archived ones in `archive/`. */
export function exportPaths(memos: MemoSummary[]): Map<string, string> {
  return new Map(memos.map((m) => [m.id, `${m.archived ? "archive/" : ""}${fileName(m.title, m.id)}.md`]));
}

/**
 * Points `memo:<id>` links at the exported files. `up` is the way to the export's root from the
 * memo's folder. Links to memos not in the export stay as they are.
 */
export function linkExported(body: string, paths: Map<string, string>, up: string): string {
  return body.replace(/\(memo:([0-9a-f-]{36})|<memo:([0-9a-f-]{36})>/g, (link, inline?: string, auto?: string) => {
    const path = paths.get(inline ?? auto!);
    if (!path) return link;
    const target = up + path.split("/").map(encodePart).join("/");
    // `<path>` is no autolink without a scheme: name it.
    const name = path.replace(/^.*\//, "").replace(/\.md$/, "").replace(/[[\]]/g, "\\$&");
    return inline ? `(${target}` : `[${name}](${target})`;
  });
}

/** Percent-encoded, parentheses too, so the path works as a Markdown link target. */
function encodePart(part: string) {
  return encodeURIComponent(part).replace(/\(/g, "%28").replace(/\)/g, "%29");
}
