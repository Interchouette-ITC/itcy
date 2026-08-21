// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

/** Pure helpers for Brave X ship URL resolve (unit-tested). */

export function statusFromHref(href) {
  if (!href) return null;
  const path = String(href).split("?")[0];
  const m = path.match(/\/(i|[A-Za-z0-9_]+)\/status\/(\d+)/);
  if (!m) return null;
  const handle = m[1].toLowerCase();
  const id = m[2];
  const url =
    handle === "i"
      ? `https://x.com/i/status/${id}`
      : `https://x.com/${handle}/status/${id}`;
  return { id, handle, url };
}

/** Toast short links use `/i/status/<id>` — treat as ours only in toast context. */
export function asOurPostedStatus(s) {
  if (!s) return null;
  if (s.handle === "interchouette") return s;
  if (s.handle === "i") {
    return {
      id: s.id,
      handle: "interchouette",
      url: `https://x.com/Interchouette/status/${s.id}`,
    };
  }
  return null;
}

/** Drop a bare status URL that duplicates an attached cited status. */
export function stripQuotedStatusUrl(text, qid) {
  const id = String(qid || "").trim();
  if (!id) return text;
  const lines = String(text || "")
    .split("\n")
    .filter((line) => {
      const t = line.trim();
      if (!t.startsWith("https://")) return true;
      const s = statusFromHref(t);
      return !(s && s.id === id);
    });
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  while (lines.length && !lines[0].trim()) lines.shift();
  return lines.join("\n");
}

export function statusIdNewer(a, b) {
  try {
    return BigInt(a) > BigInt(b);
  } catch {
    return String(a) > String(b);
  }
}

function ownStatusFromHrefs(hrefs) {
  for (const href of hrefs || []) {
    const s = statusFromHref(href);
    if (s && s.handle === "interchouette") return s;
  }
  return null;
}

/**
 * Profile Posts: skip pinned, take our newest card (DOM order = newest first).
 * Optional beforeId: must be strictly newer (so a failed Post does not claim an old tweet).
 * excludeIds: never return these (e.g. cited status id).
 */
export function pickLatestOwnPost(scan, excludeIds, beforeId) {
  const skip = new Set((excludeIds || []).map(String));
  const toast = asOurPostedStatus(statusFromHref(scan && scan.toast));
  if (toast && !skip.has(toast.id)) {
    if (!beforeId || statusIdNewer(toast.id, beforeId)) return toast;
  }

  for (const art of (scan && scan.articles) || []) {
    if (art.pinned) continue;
    const own = ownStatusFromHrefs(art.statusHrefs);
    if (!own || skip.has(own.id)) continue;
    if (beforeId && !statusIdNewer(own.id, beforeId)) continue;
    return { ...own, snippet: art.snippet || "" };
  }
  return null;
}
