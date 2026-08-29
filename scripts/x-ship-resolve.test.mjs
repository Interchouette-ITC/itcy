// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  asOurPostedStatus,
  detectPostRejectReason,
  pickLatestOwnPost,
  resolvePostedStatus,
  statusFromHref,
  statusIdNewer,
  stripQuotedStatusUrl,
} from "./x-ship-resolve.mjs";

test("detectPostRejectReason catches duplicate-post copy", () => {
  const t =
    "It looks like you already said that! Let's give other folks a chance to say their piece. Wait a little while before you post again. Not helpful?";
  assert.match(detectPostRejectReason(t), /already said that/i);
  assert.equal(detectPostRejectReason("Home timeline For you Following"), null);
});

test("XPOST-094 clickPost must not re-submit root after Control+Enter", () => {
  const src = fs.readFileSync(
    fileURLToPath(new URL("./post-twitter.mjs", import.meta.url)),
    "utf8"
  );
  const m = src.match(
    /async function clickPost\([\s\S]*?\nasync function scanProfileTimeline/
  );
  assert.ok(m, "clickPost missing");
  const body = m[0];
  assert.match(body, /Control\+Enter/);
  assert.equal((body.match(/btn\.click/g) || []).length, 0);
  assert.match(src, /postReply\(page,\s*replyText,\s*found/);
});

test("XPOST-095 first pass: one Brave session posts root then reply (no CDP close)", () => {
  const src = fs.readFileSync(
    fileURLToPath(new URL("./post-twitter.mjs", import.meta.url)),
    "utf8"
  );
  // CDP browser.close() kills Brave mid root→reply.
  assert.equal((src.match(/browser\.close\(/g) || []).length, 0);
  assert.match(src, /overflow reply file empty/);
  assert.equal(/findTimelineRoot|timelineLooksLikeRoot|ALREADY_SAID/.test(src), false);
  const rootThenReply = src.indexOf("rootToastHref = await clickPost");
  const replyCall = src.indexOf("postReply(page, replyText, found");
  assert.ok(rootThenReply > 0 && replyCall > rootThenReply, "root Post then reply");
});

test("ship scripts take brave.lock (no concurrent CDP steal)", () => {
  for (const name of [
    "post-twitter.sh",
    "fetch-twitter-pulse.sh",
    "fetch-twitter-status.sh",
  ]) {
    const src = fs.readFileSync(
      fileURLToPath(new URL(`./${name}`, import.meta.url)),
      "utf8"
    );
    assert.match(src, /twitter_brave_acquire_lock/, name);
    assert.match(src, /twitter_brave_pick_cdp_port/, name);
    assert.equal(
      /CDP_PORT="\$\{ITCY_TWITTER_CDP_PORT:-9224\}"/.test(src),
      false,
      `${name} must not hard-bind stale :9224`
    );
  }
});

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

test("resolvePostedStatus prefers clickPost toast over empty/stale profile", () => {
  // Regression: post-twitter used to navigate to profile before reading toast,
  // then fail with "no newer own tweet" even though Post succeeded.
  const before = "2090601395795718223";
  const ours = "2090700000000000001";
  const cited = "2089199040403820928";
  const found = resolvePostedStatus({
    toastHref: `/i/status/${ours}`,
    scan: {
      toast: null,
      articles: [
        {
          pinned: false,
          snippet: "old GPU tweet still on top of stale DOM",
          statusHrefs: [`/Interchouette/status/${before}`],
        },
      ],
    },
    excludeIds: [cited],
    beforeId: before,
  });
  assert.equal(found.id, ours);
  assert.equal(found.via, "toast");
  assert.ok(statusIdNewer(found.id, before));
});

test("resolvePostedStatus root then reply: reply must be newer than parent", () => {
  const root = "2090700000000000001";
  const reply = "2090700000000000002";
  const cited = "2089199040403820928";
  const replyFound = resolvePostedStatus({
    toastHref: `/i/status/${reply}`,
    scan: { toast: null, articles: [] },
    excludeIds: [cited, root],
    beforeId: root,
  });
  assert.equal(replyFound.id, reply);
  assert.equal(replyFound.via, "toast");
  assert.ok(statusIdNewer(replyFound.id, root));
});

test("resolvePostedStatus falls back to profile when toast missing", () => {
  const before = "2090000000000000000";
  const ours = "2091000000000000000";
  const found = resolvePostedStatus({
    toastHref: null,
    scan: {
      toast: null,
      articles: [
        {
          pinned: false,
          snippet: "new",
          statusHrefs: [`/Interchouette/status/${ours}`],
        },
      ],
    },
    excludeIds: [],
    beforeId: before,
  });
  assert.equal(found.id, ours);
  assert.equal(found.via, "profile");
});

test("resolvePostedStatus ignores toast that is not newer than beforeId", () => {
  const before = "2091000000000000000";
  const found = resolvePostedStatus({
    toastHref: `/i/status/${before}`,
    scan: { toast: null, articles: [] },
    excludeIds: [],
    beforeId: before,
  });
  assert.equal(found, null);
});
