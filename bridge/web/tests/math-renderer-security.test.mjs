// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const mermaidRequire = createRequire(require.resolve("mermaid"));
const katex = mermaidRequire("katex");

for (const output of ["mathml", "htmlAndMathml"]) {
  test(`Mermaid's resolved renderer supports its ${output} options`, () => {
    const rendered = katex.renderToString(String.raw`\frac{x^2}{y}`, {
      throwOnError: true,
      displayMode: true,
      output,
    });
    assert.match(rendered, /<math\b/);
    assert.match(rendered, /<mfrac>/);
    assert.match(rendered, /<msup>/);
    assert.equal(rendered.includes('class="katex-html"'), output === "htmlAndMathml");
    assert.throws(() => katex.renderToString(String.raw`\frac{`, {
      throwOnError: true, displayMode: true, output,
    }), katex.ParseError);
  });

  test(`${output} ignores inherited trust for links and external resources`, () => {
    for (const expression of [
      String.raw`\href{javascript:alert(1)}{unsafe}`,
      String.raw`\includegraphics{https://example.invalid/pixel}`,
    ]) {
      const options = Object.assign(Object.create({ trust: true }), {
        throwOnError: true, displayMode: true, output,
      });
      const rendered = katex.renderToString(expression, options);
      assert.doesNotMatch(rendered, /<(?:a|img)\b/i);
      assert.doesNotMatch(rendered, /\b(?:href|src)\s*=/i);
    }
  });
}

test("explicit trust still works without inheriting renderer configuration", () => {
  const expression = String.raw`\href{https://example.invalid/docs}{documentation}`;
  const inherited = katex.renderToString(expression, Object.create({ trust: true }));
  assert.doesNotMatch(inherited, /<a\b/);
  const explicit = katex.renderToString(expression, { trust: true });
  assert.match(explicit, /<a\b[^>]*href="https:\/\/example\.invalid\/docs"/);
});
