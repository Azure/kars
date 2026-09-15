// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer, type Server } from 'node:http';
import {
  context,
  metrics,
  propagation,
  ProxyTracerProvider,
  trace,
} from '@opentelemetry/api';
import { MeterProvider } from '@opentelemetry/sdk-metrics';
import { NodeTracerProvider } from '@opentelemetry/sdk-trace-node';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { _resetForTests, initTelemetry } from '../src/otel';

interface ExportRequest {
  path: string | undefined;
  body: unknown;
}

let server: Server | undefined;
let tracerProvider: NodeTracerProvider | undefined;
let meterProvider: MeterProvider | undefined;
const requests: ExportRequest[] = [];
const collectorErrors: unknown[] = [];

async function startCollector(port: number): Promise<string> {
  server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on('data', (chunk: Buffer) => chunks.push(chunk));
    request.on('error', (error) => collectorErrors.push(error));
    request.on('end', () => {
      try {
        const body: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'));
        requests.push({ path: request.url, body });
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end('{}');
      } catch (error) {
        collectorErrors.push(error);
        response.writeHead(400).end();
      }
    });
  });
  const collector = server;
  await new Promise<void>((resolve, reject) => {
    collector.once('error', reject);
    collector.listen(port, '127.0.0.1', resolve);
  });
  const address = collector.address();
  if (!address || typeof address === 'string') {
    throw new Error('OTLP test collector has no TCP address');
  }
  return `http://127.0.0.1:${address.port}`;
}

beforeEach(() => {
  requests.length = 0;
  collectorErrors.length = 0;
  for (const key of [
    'OTEL_EXPORTER_OTLP_ENDPOINT',
    'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT',
    'OTEL_EXPORTER_OTLP_METRICS_ENDPOINT',
    'OTEL_EXPORTER_OTLP_HEADERS',
    'OTEL_EXPORTER_OTLP_TRACES_HEADERS',
    'OTEL_EXPORTER_OTLP_METRICS_HEADERS',
  ]) {
    vi.stubEnv(key, '');
  }
  vi.stubEnv('OTEL_TRACES_SAMPLER', 'always_on');
  vi.stubEnv('OTEL_METRIC_EXPORT_INTERVAL', '60000');
  vi.spyOn(console, 'warn').mockImplementation(() => {});
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(async () => {
  try {
    await Promise.all([
      tracerProvider?.shutdown(),
      meterProvider?.shutdown(),
    ]);
  } finally {
    tracerProvider = undefined;
    meterProvider = undefined;
    trace.disable();
    metrics.disable();
    context.disable();
    propagation.disable();
    _resetForTests();
    vi.unstubAllEnvs();
    vi.restoreAllMocks();
    if (server?.listening) {
      const collector = server;
      collector.closeAllConnections();
      await new Promise<void>((resolve, reject) => {
        collector.close((error) => error ? reject(error) : resolve());
      });
    }
    server = undefined;
  }
});

describe('OpenTelemetry 2.x initialization and OTLP export', () => {
  it.each([
    {
      name: 'explicit endpoints override signal-specific and general environment',
      port: 0,
      explicit: true,
      signalEnv: true,
      generalEnv: true,
      tracesPath: '/explicit-traces',
      metricsPath: '/explicit-metrics',
      serviceVersion: '2.3.4',
    },
    {
      name: 'signal-specific environment overrides the general endpoint',
      port: 0,
      explicit: false,
      signalEnv: true,
      generalEnv: true,
      tracesPath: '/signal-traces',
      metricsPath: '/signal-metrics',
      serviceVersion: '2.3.4',
    },
    {
      name: 'general endpoint remains verbatim for traces; metrics use their default',
      port: 8443,
      explicit: false,
      signalEnv: false,
      generalEnv: true,
      tracesPath: '/general',
      metricsPath: '/v1/metrics',
      serviceVersion: undefined,
    },
    {
      name: 'empty environment falls back to the router loopback endpoints',
      port: 8443,
      explicit: false,
      signalEnv: false,
      generalEnv: false,
      tracesPath: '/v1/traces',
      metricsPath: '/v1/metrics',
      serviceVersion: undefined,
    },
  ])('$name', async (testCase) => {
    const endpoint = await startCollector(testCase.port);
    if (testCase.generalEnv) {
      vi.stubEnv('OTEL_EXPORTER_OTLP_ENDPOINT', `${endpoint}/general`);
    }
    if (testCase.signalEnv) {
      vi.stubEnv('OTEL_EXPORTER_OTLP_TRACES_ENDPOINT', `${endpoint}/signal-traces`);
      vi.stubEnv('OTEL_EXPORTER_OTLP_METRICS_ENDPOINT', `${endpoint}/signal-metrics`);
    }
    await initTelemetry({
      serviceName: 'langgraph-export-regression',
      serviceVersion: testCase.serviceVersion,
      tracesEndpoint: testCase.explicit ? `${endpoint}/explicit-traces` : undefined,
      metricsEndpoint: testCase.explicit ? `${endpoint}/explicit-metrics` : undefined,
    });

    const globalTracerProvider = trace.getTracerProvider();
    expect(globalTracerProvider).toBeInstanceOf(ProxyTracerProvider);
    if (!(globalTracerProvider instanceof ProxyTracerProvider)) {
      throw new Error('initTelemetry did not register a tracer provider');
    }
    const delegate = globalTracerProvider.getDelegate();
    if (!(delegate instanceof NodeTracerProvider)) {
      throw new Error('initTelemetry did not initialize the real NodeTracerProvider');
    }
    tracerProvider = delegate;
    const globalMeterProvider = metrics.getMeterProvider();
    if (!(globalMeterProvider instanceof MeterProvider)) {
      throw new Error('initTelemetry did not initialize the real MeterProvider');
    }
    meterProvider = globalMeterProvider;
    expect(console.warn).not.toHaveBeenCalled();

    await initTelemetry({
      serviceName: 'must-not-replace-first-initialization',
      serviceVersion: '9.9.9',
      tracesEndpoint: `${endpoint}/must-not-export-traces`,
      metricsEndpoint: `${endpoint}/must-not-export-metrics`,
    });
    expect(trace.getTracerProvider()).toBe(globalTracerProvider);
    expect(globalTracerProvider.getDelegate()).toBe(tracerProvider);
    expect(metrics.getMeterProvider()).toBe(meterProvider);
    expect(console.error).toHaveBeenCalledTimes(1);

    trace.getTracer('langgraph-regression').startSpan('real-sdk-span').end();
    metrics.getMeter('langgraph-regression').createCounter('real_sdk_counter').add(3);
    await tracerProvider.forceFlush();
    await meterProvider.forceFlush();

    expect(collectorErrors).toEqual([]);
    expect(requests.map((request) => request.path).sort()).toEqual(
      [testCase.tracesPath, testCase.metricsPath].sort(),
    );
    const attributes = expect.arrayContaining([
      { key: 'service.name', value: { stringValue: 'langgraph-export-regression' } },
      { key: 'service.version', value: { stringValue: testCase.serviceVersion ?? '0.1.0' } },
      { key: 'service.namespace', value: { stringValue: 'kars' } },
      { key: 'kars.runtime.kind', value: { stringValue: 'LangGraph' } },
      { key: 'kars.runtime.language', value: { stringValue: 'typescript' } },
    ]);
    expect(requests.find((request) => request.path === testCase.tracesPath)?.body)
      .toMatchObject({
        resourceSpans: [{
          resource: { attributes },
          scopeSpans: expect.arrayContaining([expect.objectContaining({
            scope: expect.objectContaining({ name: 'langgraph-regression' }),
            spans: expect.arrayContaining([
              expect.objectContaining({ name: 'real-sdk-span' }),
            ]),
          })]),
        }],
      });
    expect(requests.find((request) => request.path === testCase.metricsPath)?.body)
      .toMatchObject({
        resourceMetrics: [{
          resource: { attributes },
          scopeMetrics: expect.arrayContaining([expect.objectContaining({
            scope: expect.objectContaining({ name: 'langgraph-regression' }),
            metrics: expect.arrayContaining([expect.objectContaining({
              name: 'real_sdk_counter',
              sum: expect.objectContaining({
                dataPoints: expect.arrayContaining([
                  expect.objectContaining({ asDouble: 3 }),
                ]),
              }),
            })]),
          })]),
        }],
      });
    expect(console.warn).not.toHaveBeenCalled();
  });
});
