// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import test from "node:test";
import {
  asOurPostedStatus,
  pickLatestOwnPost,
  statusFromHref,
  stripQuotedStatusUrl,
} from "./lib/x-ship-resolve.mjs";

test("statusFromHref keeps /i/ as handle i", () => {
  const s = statusFromHref("/i/status/2090577804115021919");
  assert.equal(s.handle, "i");
  assert.equal(s.id, "2090577804115021919");
});

test("asOurPostedStatus accepts toast short link as ours", () => {
  const ours = asOurPostedStatus(
    statusFromHref("https://x.com/i/status/2090577804115021919")
  );
  assert.equal(ours.handle, "interchouette");
  assert.equal(
    ours.url,
    "https://x.com/Interchouette/status/2090577804115021919"
  );
});

test("stripQuotedStatusUrl drops only the cited id line", () => {
  const text = [
    "📜 Rust GUI just got a GPU-powered upgrade.",
    "",
    "#Rust #GUI #OpenSource",
    "",
    "https://x.com/milonspace/status/2089661151529574481",
  ].join("\n");
  const out = stripQuotedStatusUrl(text, "2089661151529574481");
  assert.ok(out.includes("Rust GUI"));
  assert.ok(!out.includes("2089661151529574481"));
});

test("pickLatestOwnPost takes first non-pinned own card (no text search)", () => {
  const before = "2090000000000000000";
  const ours = "2091000000000000000";
  const cited = "2089661151529574481";
  const found = pickLatestOwnPost(
    {
      toast: null,
      articles: [
        {
          pinned: true,
          snippet: "Pinned old thing",
          statusHrefs: ["/Interchouette/status/100"],
        },
        {
          pinned: false,
          snippet: "whatever X rendered",
          statusHrefs: [
            `/milonspace/status/${cited}`,
            `/Interchouette/status/${ours}`,
          ],
        },
        {
          pinned: false,
          snippet: "older",
          statusHrefs: [`/Interchouette/status/${before}`],
        },
      ],
    },
    [cited],
    before
  );
  assert.equal(found.id, ours);
});

test("pickLatestOwnPost refuses old timeline when Post did not create a newer id", () => {
  const before = "2091000000000000000";
  const found = pickLatestOwnPost(
    {
      toast: null,
      articles: [
        {
          pinned: false,
          snippet: "still the previous top tweet",
          statusHrefs: [`/Interchouette/status/${before}`],
        },
      ],
    },
    [],
    before
  );
  assert.equal(found, null);
});

test("pickLatestOwnPost uses toast when present and newer", () => {
  const before = "2090000000000000000";
  const ours = "2092000000000000000";
  const found = pickLatestOwnPost(
    { toast: `/i/status/${ours}`, articles: [] },
    [],
    before
  );
  assert.equal(found.id, ours);
  assert.equal(found.handle, "interchouette");
});
