// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
import {
  activeComposerScope,
  composerBox,
  composerText,
  profilePostButton,
  scanProfileTimeline,
  submitPostButton,
} from "./x-ship-ui.mjs";

test("composerText reads textarea values, not their empty innerText", async () => {
  const text = "Rust.\n\n`code` \u{1f989}";
  for (const element of [
    { value: text, innerText: "" },
    { innerText: text },
  ]) {
    assert.equal(await composerText({ evaluate: (fn) => fn(element) }), text);
  }
});

test("legacy and redesigned X controls", {
  skip: !process.env.PLAYWRIGHT_REQUIRE_FROM &&
    "Set PLAYWRIGHT_REQUIRE_FROM to run offline browser fixtures",
}, async (t) => {
  const { chromium } = createRequire(process.env.PLAYWRIGHT_REQUIRE_FROM)("playwright");
  const browser = await chromium.launch({
    headless: true,
    executablePath: process.env.ITCY_BROWSER_EXECUTABLE || undefined,
  });
  try {
    const page = await browser.newPage();
    await page.route("**/*", (route) => route.abort());
    await t.test("redesigned sidebar excludes reply and quote links", async () => {
      await page.setContent(`
        <a href="/compose/post?in_reply_to=123">Reply</a>
        <a href="/compose/post?quote=456">Quote</a>
        <a id="sidebar-compose-post" aria-label="Post" href="/compose/post">Post</a>
      `);
      assert.equal(await profilePostButton(page).getAttribute("id"), "sidebar-compose-post");
      await page.locator("#sidebar-compose-post").evaluate((el) => el.removeAttribute("id"));
      assert.equal(await profilePostButton(page).getAttribute("href"), "/compose/post");
    });
    await t.test("redesigned dialog uses a textarea and native disabled button", async () => {
      await page.setContent(`
        <button data-testid="tweetButtonInline">Post</button>
        <textarea aria-label="What's happening?" hidden></textarea>
        <div role="dialog">
          <textarea aria-label="What's happening?" placeholder="What's happening?"></textarea>
          <button>Schedule post</button><button disabled>Post</button>
        </div>
      `);
      const dialog = page.locator('[role="dialog"]').filter({ has: composerBox(page) });
      assert.equal(await dialog.count(), 1);
      const box = composerBox(dialog);
      const payload = "Rust.\n\n`model.onnx` \u{1f989}";
      await box.fill(payload);
      assert.equal(await composerText(box), payload);
      const button = submitPostButton(dialog);
      assert.equal(await button.innerText(), "Post");
      assert.equal(await button.isEnabled(), false);
      await button.evaluate((el) => { el.disabled = false; });
      assert.equal(await button.isEnabled(), true);
    });
    await t.test("legacy contenteditable and test IDs still work", async () => {
      await page.setContent(`
        <a data-testid="SideNav_NewTweet_Button" href="/compose/tweet">Post</a>
        <div role="dialog">
          <div contenteditable="true" style="white-space: pre-wrap" data-testid="tweetTextarea_0"></div>
          <button data-testid="tweetButton" aria-disabled="true">Post</button>
        </div>
      `);
      assert.equal(await profilePostButton(page).getAttribute("href"), "/compose/tweet");
      const dialog = page.locator('[role="dialog"]').filter({ has: composerBox(page) });
      const box = composerBox(dialog);
      await box.fill("Legacy text.");
      assert.equal(await composerText(box), "Legacy text.");
      assert.equal(await submitPostButton(dialog).isEnabled(), false);
    });
    await t.test("reply dialog selects Reply rather than background Post", async () => {
      await page.setContent(`
        <button>Post</button>
        <div role="dialog"><textarea placeholder="What's happening?"></textarea>
          <button>Reply</button>
        </div>
      `);
      const dialog = page.locator('[role="dialog"]').filter({ has: composerBox(page) });
      assert.equal(await submitPostButton(dialog).innerText(), "Reply");
    });
    await t.test("live redesigned reply label works without root-post placeholder or test IDs", async () => {
      await page.setContent(`
        <textarea aria-label="Post your reply" placeholder="Post your reply" rows="1"></textarea>
        <button disabled>Reply</button>
        <div role="dialog">
          <textarea aria-label="Post your reply" aria-multiline="true"
            placeholder="Post your reply" rows="4"></textarea>
          <button disabled>Reply</button>
        </div>
        <script>
          document.querySelector('[role="dialog"] textarea').addEventListener("input", (event) => {
            document.querySelector('[role="dialog"] button').disabled = !event.target.value.trim();
          });
        </script>
      `);
      const scope = await activeComposerScope(page);
      assert.notEqual(scope, page, "reply dialog must be recognized");
      const payload = "Full second tweet.\n\nReply with \u{1f989} and \`code\`.";
      await composerBox(scope).fill(payload);
      assert.equal(await composerText(composerBox(scope)), payload);
      assert.equal(await page.locator("body > textarea").inputValue(), "");
      assert.equal(await submitPostButton(scope).isEnabled(), true);
    });
    for (const legacy of [false, true]) {
      await t.test(`reply scopes text and submit together (${legacy ? "legacy" : "redesigned"})`, async () => {
        const composer = legacy
          ? '<div contenteditable="true" style="white-space: pre-wrap" data-testid="tweetTextarea_0"></div>'
          : '<textarea placeholder="What\'s happening?"></textarea>';
        await page.setContent(`
          <section id="inline">
            ${composer}
            <button data-testid="tweetButtonInline" disabled>Reply</button>
          </section>
          <div role="dialog">
            ${composer}
            <button ${legacy ? 'data-testid="tweetButton"' : ""} disabled>Reply</button>
          </div>
          <script>
            document.querySelector('[role="dialog"]').addEventListener("input", (event) => {
              const text = event.target.value || event.target.innerText;
              document.querySelector('[role="dialog"] button').disabled = !text.trim();
            });
          </script>
        `);
        const scope = await activeComposerScope(page);
        assert.equal(await scope.getAttribute("role"), "dialog");
        const payload = legacy
          ? "Second tweet. Complete reply with \u{1f989} and \`code\`."
          : "Second tweet.\n\nComplete reply with \u{1f989} and \`code\`.";
        if (legacy) {
          await composerBox(scope).click();
          await page.keyboard.insertText(payload);
        } else {
          await composerBox(scope).fill(payload);
        }
        assert.equal(await composerText(composerBox(scope)), payload);
        assert.equal(await composerText(composerBox(page.locator("#inline"))), "");
        assert.equal(await submitPostButton(scope).isEnabled(), true);
        assert.equal(await page.locator("#inline button").isEnabled(), false);
      });
    }
    await t.test("inline reply remains available without a visible dialog", async () => {
      await page.setContent(`
        <textarea placeholder="What's happening?"></textarea><button>Reply</button>
        <div role="dialog" hidden>
          <textarea placeholder="What's happening?"></textarea><button disabled>Reply</button>
        </div>
      `);
      const scope = await activeComposerScope(page);
      assert.equal(scope, page);
      await composerBox(scope).fill("Inline reply.");
      assert.equal(await composerText(composerBox(scope)), "Inline reply.");
      assert.equal(await submitPostButton(scope).isEnabled(), true);
    });
    await t.test("redesigned timeline pairs full body with permalink, not analytics or link card", async () => {
      await page.setContent(`
        <article>
          <a href="/Interchouette/status/123">1h</a>
          <div dir="auto" lang="en">Complete reply.

#Rust

<a href="https://t.co/abc" title="https://example.com/full-article">example.com/full…</a></div>
          <a href="/Interchouette/status/123/analytics">20 Views</a>
          <div>Unrelated link card preview text.</div>
        </article>
      `);
      const scan = await scanProfileTimeline(page);
      assert.deepEqual(scan.articles[0].statusHrefs, ["/Interchouette/status/123"]);
      assert.equal(scan.articles[0].text, "Complete reply.\n\n#Rust\n\nhttps://example.com/full-article");
    });
  } finally {
    await browser.close();
  }
});
