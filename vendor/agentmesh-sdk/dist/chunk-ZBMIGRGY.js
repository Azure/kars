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

export { StorageNamespace, namespacedKey, stripNamespace };
//# sourceMappingURL=chunk-ZBMIGRGY.js.map
//# sourceMappingURL=chunk-ZBMIGRGY.js.map