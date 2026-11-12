// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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

          if (typeof process.send !== "function") {
            throw new Error("Storybook smoke test requires an IPC channel to report emitted assets");
          }
          process.send({
            type: "kars-storybook-smoke-assets",
            hasErrors: stats.hasErrors(),
            storyAssets: [...new Set(storyAssets)].filter((asset) => /\.m?js$/.test(asset)),
          });
        });
      },
    });
    return config;
  },
};
