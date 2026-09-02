// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import test from "node:test";
import {
  acceptPublisherHref,
  publisherHrefsFromHtml,
  tweetBodyWithHrefLinksFromHtml,
} from "./lib/tweet-href-urls.mjs";

const WASMER_ANCHOR =
  '<a class="font-chirp" href="https://wasmer.io/posts/wasmer-local-sandboxes-for-ai-agents" rel="noopener noreferrer" target="_blank">wasmer.io/posts/wasmer-l…</a>';

const WASMER_PROSE = `What if the sandbox lived next to the AI agent?

Isolated. Blazing Fast.
Embeddable from JS, Python, or Rust.
Fully runnable in the browser.

One SDK. Open source. Available today.`;

test("acceptPublisherHref keeps full wasmer slug from href not display text", () => {
  const u = acceptPublisherHref(
    "https://wasmer.io/posts/wasmer-local-sandboxes-for-ai-agents",
  );
  assert.equal(u, "https://wasmer.io/posts/wasmer-local-sandboxes-for-ai-agents");
});

test("publisherHrefsFromHtml reads href not truncated inner text", () => {
  const hrefs = publisherHrefsFromHtml(WASMER_ANCHOR);
  assert.deepEqual(hrefs, [
    "https://wasmer.io/posts/wasmer-local-sandboxes-for-ai-agents",
  ]);
});

test("tweet body uses one-line href not three-row display chrome", () => {
  const body = tweetBodyWithHrefLinksFromHtml(WASMER_PROSE, WASMER_ANCHOR);
  assert.ok(
    body.includes("https://wasmer.io/posts/wasmer-local-sandboxes-for-ai-agents"),
  );
  assert.equal(body.includes("wasmer-l\nocal"), false);
  assert.equal(body.includes("https://\nwasmer.io"), false);
  assert.equal(body.includes("…"), false);
});

test("acceptPublisherHref drops x.com and t.co", () => {
  assert.equal(acceptPublisherHref("https://x.com/wasmerio/status/1"), null);
  assert.equal(acceptPublisherHref("https://t.co/JpyB8lJ4Dz"), null);
});
