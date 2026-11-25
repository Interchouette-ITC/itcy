// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

const COMPOSER_SELECTOR =
  '[data-testid="tweetTextarea_0"], textarea[aria-label="What\'s happening?"], textarea[placeholder="What\'s happening?"]';

export function composerBox(scope) {
  return scope.locator(`:is(${COMPOSER_SELECTOR}):visible`).first();
}

export async function composerText(box) {
  return box.evaluate((el) =>
    typeof el.value === "string" ? el.value : el.innerText
  );
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
