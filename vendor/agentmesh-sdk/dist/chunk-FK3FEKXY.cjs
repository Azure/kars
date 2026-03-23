'use strict';

// src/storage/interface.ts
var StorageNamespace = {
  IDENTITY: "identity/",
  SESSIONS: "sessions/",
  PREKEYS: "prekeys/",
  AUDIT: "audit/",
  CACHE: "cache/"
};
function namespacedKey(namespace, key) {
  return `${namespace}${key}`;
}
function stripNamespace(namespace, key) {
  if (key.startsWith(namespace)) {
    return key.slice(namespace.length);
  }
  return key;
}

exports.StorageNamespace = StorageNamespace;
exports.namespacedKey = namespacedKey;
exports.stripNamespace = stripNamespace;
//# sourceMappingURL=chunk-FK3FEKXY.cjs.map
//# sourceMappingURL=chunk-FK3FEKXY.cjs.map