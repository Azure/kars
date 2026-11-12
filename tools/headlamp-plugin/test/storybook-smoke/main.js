// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

const fs = require("node:fs");

function moduleContainsFixture(module) {
  if (typeof module.resource === "string" && module.resource.includes("webpack-dev-middleware.stories.js")) {
    return true;
  }
  return module.modules ? [...module.modules].some(moduleContainsFixture) : false;
}

module.exports = {
  framework: {
    name: "@storybook/react-webpack5",
    options: {},
  },
  stories: ["./*.stories.js"],
  webpackFinal: async (config) => {
    config.plugins ??= [];
    config.plugins.push({
      apply(compiler) {
        compiler.hooks.done.tap("KarsStorybookSmokeAssetManifestPlugin", (stats) => {
          const storyAssets = [];
          for (const chunk of stats.compilation.chunks) {
            const containsFixture = [...stats.compilation.chunkGraph.getChunkModulesIterable(chunk)]
              .some(moduleContainsFixture);
            if (containsFixture) {
              storyAssets.push(...chunk.files);
            }
          }

          const manifestPath = process.env.KARS_STORYBOOK_SMOKE_ASSET_MANIFEST;
          if (!manifestPath) {
            throw new Error("KARS_STORYBOOK_SMOKE_ASSET_MANIFEST is required for the Storybook smoke test");
          }
          const temporaryPath = `${manifestPath}.tmp`;
          fs.writeFileSync(temporaryPath, JSON.stringify({
            hasErrors: stats.hasErrors(),
            storyAssets: [...new Set(storyAssets)].filter((asset) => /\.m?js$/.test(asset)),
          }));
          fs.renameSync(temporaryPath, manifestPath);
        });
      },
    });
    return config;
  },
};
