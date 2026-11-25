// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

const COMPOSER_SELECTOR =
  '[data-testid="tweetTextarea_0"], textarea[aria-label="What\'s happening?"], textarea[placeholder="What\'s happening?"], textarea[aria-label="Post your reply"], textarea[placeholder="Post your reply"]';

export function composerBox(scope) {
  return scope.locator(`:is(${COMPOSER_SELECTOR}):visible`).first();
}

export async function activeComposerScope(page) {
  const dialog = page
    .locator('[role="dialog"]:visible')
    .filter({ has: composerBox(page) })
    .first();
  return (await dialog.isVisible()) ? dialog : page;
}

export async function composerText(box) {
  return box.evaluate((el) =>
    typeof el.value === "string" ? el.value : el.innerText
  );
}

export async function scanProfileTimeline(page) {
  return page.evaluate(() => {
    const toast = document.querySelector('[data-testid="toast"] a[href*="/status/"]');
    const articles = [...document.querySelectorAll("article")].map((art) => {
      const raw = (art.innerText || "").replace(/\s+/g, " ").trim();
      const body = art.querySelector('[data-testid="tweetText"], div[dir="auto"][lang]');
      let text = "";
      if (body) {
        const copy = body.cloneNode(true);
        for (const link of copy.querySelectorAll('a[href^="https://t.co/"]')) {
          link.textContent = link.getAttribute("title") || link.href;
        }
        text = copy.textContent || "";
      }
      const statusHrefs = [...art.querySelectorAll('a[href*="/status/"]')]
        .map((a) => a.getAttribute("href"))
        .filter((href) => /^\/(?:i|[A-Za-z0-9_]+)\/status\/\d+$/.test(href));
      return {
        pinned: /pinned|épinglé/i.test(raw.slice(0, 96)),
        statusHrefs,
        snippet: raw.slice(0, 280),
        text,
      };
    });
    return { toast: toast ? toast.getAttribute("href") : null, articles };
  });
}

export function profilePostButton(page) {
  return page.locator(
    ':is([data-testid="SideNav_NewTweet_Button"], #sidebar-compose-post, a[href="/compose/post"], a[href="/compose/tweet"]):visible'
  ).first();
}

export function submitPostButton(scope) {
  return scope.locator(
    '[data-testid="tweetButton"]:visible, [data-testid="tweetButtonInline"]:visible'
  ).or(scope.getByRole("button", { name: /^(Post|Reply|Publier|R\u00e9pondre)$/i }))
    .filter({ visible: true }).first();
}
