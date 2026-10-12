// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { chromium } from "playwright";

test("native AL3 browser renders, clicks, captures and isolates contexts as UID1000", async () => {
  assert.match(await readFile("/etc/os-release", "utf8"), /^ID=azurelinux$/m);
  assert.equal(process.getuid(), 1000);
  assert.equal(process.versions.node.split(".")[0], "24");
  const browser = await chromium.launch({ headless: true, args: ["--no-sandbox"] });
  try {
    const first = await browser.newContext();
    const second = await browser.newContext();
    const page = await first.newPage();
    const other = await second.newPage();
    await page.setContent('<button id="advance">Advance</button><output>0</output>');
    await page.evaluate(() => {
      window.karsOwnedContext = true;
      document.querySelector("#advance").addEventListener("click", () => {
        document.querySelector("output").textContent = "1";
      });
    });
    assert.equal(await page.locator("output").textContent(), "0");
    await page.getByRole("button", { name: "Advance" }).click();
    assert.equal(await page.locator("output").textContent(), "1");
    assert.equal(await other.evaluate(() => window.karsOwnedContext), undefined);
    assert.equal(await other.locator("button").count(), 0);
    const screenshot = await page.screenshot();
    assert.equal(screenshot.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
    assert.ok(screenshot.length > 64);
    await first.close();
    await second.close();
  } finally {
    await browser.close();
  }
});
