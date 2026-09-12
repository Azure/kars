// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Complete normalized CRD specs: BASE365 8b206065608593667a40665b3f48225ef9ce278d
// -> 470773c2 plus the independently approved b5ad6791 evaluator-v2 additions.
// Metadata/Helm retention is not part of these schema fingerprints.
export const BASE365 = "8b206065608593667a40665b3f48225ef9ce278d";
export const MIGRATION = "kars.azure.com/sre-base365-schema/v1";
export const EVALUATOR_V2 = "f9a25ecca9292f85d0604b7eab09166cb4dfd6c5443f8175bac2664ee50e93a2";

export const CANONICAL_SCHEMAS: Readonly<Record<string, { before?: string; after: readonly string[] }>> = {
  "a2aagents.kars.azure.com": { before: "8ab113b8135124c3659acac960ba46a25b060b7f527a2e216fab38327e9608aa", after: ["8ab113b8135124c3659acac960ba46a25b060b7f527a2e216fab38327e9608aa"] },
  "egressapprovals.kars.azure.com": { before: "b42875fc207f1b2cf91cd245d949ad91c8f488273f6148295c7caf41ddaf1cfe", after: ["b42875fc207f1b2cf91cd245d949ad91c8f488273f6148295c7caf41ddaf1cfe"] },
  "inferencepolicies.kars.azure.com": { before: "cabb877390450c18717f56c03a0a6891fdb326177ecc150bb9e424c8fd8fe324", after: ["cabb877390450c18717f56c03a0a6891fdb326177ecc150bb9e424c8fd8fe324"] },
  "karsapprovals.kars.azure.com": { before: "b3b9c4d0f71c3bdc0a1167b921ac414a5fe31d6ef1b7de3ce8a3229c69231155", after: ["b3b9c4d0f71c3bdc0a1167b921ac414a5fe31d6ef1b7de3ce8a3229c69231155"] },
  "karsauthconfigs.kars.azure.com": { before: "c240ac5709140145625c9f8a8032338e5f5c839d24f25202bc32688f005cd8db", after: ["c240ac5709140145625c9f8a8032338e5f5c839d24f25202bc32688f005cd8db"] },
  "karsevals.kars.azure.com": { before: "681159e3cb91154740b8a9bd88ff5ca09ab7957e668933b4f7b57db2fe921fd6", after: ["681159e3cb91154740b8a9bd88ff5ca09ab7957e668933b4f7b57db2fe921fd6", EVALUATOR_V2] },
  "karsmemories.kars.azure.com": { before: "517031b3dfcb35e3f48673be69439e75c0fba43f70815d3591a5458e3785136e", after: ["517031b3dfcb35e3f48673be69439e75c0fba43f70815d3591a5458e3785136e"] },
  "karsprofiles.kars.azure.com": { before: "4712f6752e05586fcebda8f6c1c383b5e4cf565e634b6667c6b92f7779498c61", after: ["5c0655a48d97bc3308d2655b36d73fd710e9200801cf9bd1a028b02713e981bd"] },
  "karsreceipts.kars.azure.com": { before: "b6541d48d924996034ca4b36c10e44af61ad8b9036b88ca546343f88620c1731", after: ["b6541d48d924996034ca4b36c10e44af61ad8b9036b88ca546343f88620c1731"] },
  "karsskills.kars.azure.com": { before: "2a22afa4de149297ca80f58536a05774074d7f531695c71d9f5e8823e1dff952", after: ["2a22afa4de149297ca80f58536a05774074d7f531695c71d9f5e8823e1dff952"] },
  "karssreactions.kars.azure.com": { before: "b48ec2f96c89f327bb21e5fa87606a4531553f51f010e4c93c2e3e7b359f7478", after: ["ade98904f9966330827135686ca10a964bc4bee0cad7c2e66cf6affc35075a70"] },
  "karstasks.kars.azure.com": { before: "3803e55d0dd5f5b10de935973d472579b3dd818be968de712e86352d2d39abc2", after: ["cf9632f5b31325996922affe6375067c1cea31b0186a5941d3eb11a579eba3aa"] },
  "karsteams.kars.azure.com": { before: "a18b3836bd1a37f2dc31d249f4e412be9c439b20871aa798f06b74228b74116f", after: ["47356b37366d166c88e7898bf512cd58f84e40d14200aa3e6646973096063872"] },
  "mcpservers.kars.azure.com": { before: "67f2913e504a28d92ed2cc773f75efe4d132de4dbe304330cccca222e93264fa", after: ["4f2b2d1c8e2b01235d48d1adc5fe8f45ad2362c623e519a64788106293430f1f"] },
  "toolpolicies.kars.azure.com": { before: "f594f5d274bb23e18e6a6f34227bb28a7dd20c7ebcfae8aa7975e3edea6c90f3", after: ["f594f5d274bb23e18e6a6f34227bb28a7dd20c7ebcfae8aa7975e3edea6c90f3"] },
  "trustgraphs.kars.azure.com": { before: "354d1f2405b0dd99fd963a49b2ec7e2a2dc702abe68a9fdc9b9088d3df412bf2", after: ["354d1f2405b0dd99fd963a49b2ec7e2a2dc702abe68a9fdc9b9088d3df412bf2"] },
  "karssandboxes.kars.azure.com": { before: "d7ddb2d69dc654e3a457a4455c7de7e3f44ec42a9384a39e16816f012646e7da", after: ["da674a84c19c8ac64a1d96d04f79435c6899601426e25931feaca483f139b920"] },
  "karspairings.kars.azure.com": { before: "18dd892fc268f575d67e44456a3031885645561c6b9ec1ae995faa659c8b2920", after: ["18dd892fc268f575d67e44456a3031885645561c6b9ec1ae995faa659c8b2920"] },
  "karsbudgetaccounts.kars.azure.com": { after: ["0706c8eb2b31308de59f6b388cf9744a0989ccdd7cc6173ef42c3eb331d91135"] },
  "karscredentialgrants.kars.azure.com": { after: ["5427f9dd6735d79b069abb24398161650c0dc56eed9dfc07dc92c094b4976b95"] },
  "karssreregistrations.kars.azure.com": { after: ["23f69477cb6819ac7c8cea99af3d044a0cce8712468351400c57a571e7d0034a"] },
};

for (const transition of Object.values(CANONICAL_SCHEMAS)) {
  Object.freeze(transition.after);
  Object.freeze(transition);
}
Object.freeze(CANONICAL_SCHEMAS);
