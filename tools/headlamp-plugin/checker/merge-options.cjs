// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

'use strict';

const { deepmergeCustom } = require('deepmerge-ts');

// Keep the checker's existing value/array cloning semantics, but never assign
// attacker-controlled property names through Object.prototype setters.
function cloneOption(value) {
  if (value === null || typeof value !== 'object') return value;
  const tag = Object.prototype.toString.call(value);
  if (tag === '[object Date]' || tag === '[object RegExp]' ||
      value.$$typeof === Symbol.for('react.element')) return value;
  if (Array.isArray(value)) return value.map(cloneOption);
  const result = {};
  for (const key of Reflect.ownKeys(value)) {
    if (Object.prototype.propertyIsEnumerable.call(value, key)) {
      Object.defineProperty(result, key, {
        value: cloneOption(value[key]), enumerable: true, writable: true, configurable: true,
      });
    }
  }
  return result;
}

const merge = deepmergeCustom({
  filterValues: false,
  mergeRecords: (values, utils, meta) => {
    let lastElement = -1;
    for (let index = 0; index < values.length; index++) {
      if (values[index].$$typeof === Symbol.for('react.element')) lastElement = index;
    }
    if (lastElement === values.length - 1) return values[lastElement];
    return utils.defaultMergeFunctions.mergeRecords(values.slice(lastElement + 1), utils, meta);
  },
  mergeArrays: values => values.reduce((result, value) => result.concat(value), []).map(cloneOption),
  mergeOthers: values => cloneOption(values[values.length - 1]),
});

module.exports = (externalOptions, options) => merge(cloneOption(externalOptions), cloneOption(options));
