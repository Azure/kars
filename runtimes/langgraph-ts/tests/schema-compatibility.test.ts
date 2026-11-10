// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tool } from '@langchain/core/tools';
import { END, START, StateGraph } from '@langchain/langgraph';
import { z } from 'zod';
import { describe, expect, it } from 'vitest';

const require = createRequire(import.meta.url);

describe('patched supported Zod peer', () => {
  it('resolves the patched peer for the adapter and both LangChain packages', () => {
    const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
    const lock = JSON.parse(readFileSync(new URL('../package-lock.json', import.meta.url), 'utf8'));
    expect(manifest.dependencies.zod).toBe('^4.6.6');
    const copies = Object.entries(lock.packages).filter(([path]) => path.endsWith('node_modules/zod'));
    expect(copies).toHaveLength(1);
    expect(copies[0]?.[1]).toMatchObject({ version: '4.6.7', resolved: 'https://registry.npmjs.org/zod/-/zod-4.6.7.tgz', integrity: expect.stringMatching(/^sha512-/) });
    for (const parent of ['@langchain/core', '@langchain/langgraph']) {
      expect(createRequire(require.resolve(parent))('zod/package.json').version).toBe('4.6.7');
    }
  });

  it('validates real structured tool arguments and rejects oversized collections before invocation', async () => {
    let invocations = 0;
    const join = tool(async ({ items }) => {
      invocations++;
      return items.join(',');
    }, { name: 'join', description: 'Join bounded items', schema: z.object({ items: z.array(z.string()).max(2) }) });
    await expect(join.invoke({ items: ['one', 'two'] })).resolves.toBe('one,two');
    await expect(join.invoke({ items: ['one', 'two', 'three'] })).rejects.toThrow();
    expect(invocations).toBe(1);
  });

  it('executes a real LangGraph using a Zod 4 state schema', async () => {
    const graph = new StateGraph(z.object({ count: z.number() }))
      .addNode('increment', state => ({ count: state.count + 1 }))
      .addEdge(START, 'increment')
      .addEdge('increment', END)
      .compile();
    await expect(graph.invoke({ count: 2 })).resolves.toEqual({ count: 3 });
  });
});
