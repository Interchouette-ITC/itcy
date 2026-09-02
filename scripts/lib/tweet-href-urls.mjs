// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

/**
 * Keep publisher cite hrefs from tweet <a> tags. X truncates link text; href is canonical.
 *
 * @param {string | null | undefined} href
 * @returns {string | null}
 */
export function acceptPublisherHref(href) {
  if (!href || typeof href !== "string") return null;
  const u = href.trim().split("?")[0].split("#")[0];
  if (!u.startsWith("http://") && !u.startsWith("https://")) return null;
  let host = "";
  try {
    host = new URL(u).hostname.toLowerCase();
  } catch {
    return null;
  }
  if (host === "x.com" || host === "twitter.com" || host === "t.co") return null;
  return u;
}

/**
 * Publisher hrefs from a tweet article (browser DOM).
 *
 * @param {Element | null | undefined} article
 * @returns {string[]}
 */
export function collectPublisherHrefsFromArticle(article) {
  if (!article) return [];
  const out = [];
  const seen = new Set();
  const root = article.querySelector('[data-testid="tweetText"]') || article;
  for (const a of root.querySelectorAll("a[href]")) {
    const u = acceptPublisherHref(a.getAttribute("href"));
    if (u && !seen.has(u)) {
      seen.add(u);
      out.push(u);
    }
  }
  for (const a of article.querySelectorAll('[data-testid="card.wrapper"] a[href]')) {
    const u = acceptPublisherHref(a.getAttribute("href"));
    if (u && !seen.has(u)) {
      seen.add(u);
      out.push(u);
    }
  }
  return out;
}

/**
 * Tweet body for digest/Slack: replace publisher <a> display text with href (one line).
 *
 * @param {Element | null | undefined} root tweetText node or article
 * @returns {string}
 */
export function tweetBodyWithHrefLinks(root) {
  if (!root) return "";
  const tw = root.matches?.('[data-testid="tweetText"]')
    ? root
    : root.querySelector?.('[data-testid="tweetText"]') || root;
  if (!tw || typeof tw.cloneNode !== "function") return "";
  const clone = tw.cloneNode(true);
  for (const a of clone.querySelectorAll("a[href]")) {
    const u = acceptPublisherHref(a.getAttribute("href"));
    if (u) {
      a.replaceWith(document.createTextNode(u));
    }
  }
  return (clone.innerText || "").replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

/**
 * Test helper: publisher hrefs from tweet HTML fixture.
 *
 * @param {string} html
 * @returns {string[]}
 */
export function publisherHrefsFromHtml(html) {
  const out = [];
  const seen = new Set();
  const re = /<a\b[^>]*\bhref=["']([^"']+)["']/gi;
  let m = re.exec(html);
  while (m) {
    const u = acceptPublisherHref(m[1]);
    if (u && !seen.has(u)) {
      seen.add(u);
      out.push(u);
    }
    m = re.exec(html);
  }
  return out;
}

/**
 * Test helper: body after substituting publisher anchor text with href.
 *
 * @param {string} prose
 * @param {string} anchorHtml
 * @returns {string}
 */
export function tweetBodyWithHrefLinksFromHtml(prose, anchorHtml) {
  const hrefs = publisherHrefsFromHtml(anchorHtml);
  const url = hrefs[0] || "";
  const prosePart = (prose || "").trim();
  if (!url) return prosePart;
  if (!prosePart) return url;
  return `${prosePart}\n\n${url}`;
}
