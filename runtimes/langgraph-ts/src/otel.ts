// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * OpenTelemetry auto-init for the LangGraph (TypeScript) adapter.
 *
 * The router sidecar exposes an OTLP/HTTP collector at
 * `/v1/traces` and `/v1/metrics`. We default to the loopback address
 * so egress-guard can keep the pod sealed; an operator may override
 * via `OTEL_EXPORTER_OTLP_ENDPOINT` if they front the collector
 * elsewhere.
 *
 * `initTelemetry()` is idempotent: a second call is a no-op once a
 * tracer/meter provider is set globally.
 */

const DEFAULT_OTLP_TRACES_ENDPOINT = 'http://127.0.0.1:8443/v1/traces';
const DEFAULT_OTLP_METRICS_ENDPOINT = 'http://127.0.0.1:8443/v1/metrics';

let initialized = false;

function otlpEndpoint(envVar: string, fallback: string): string {
  const raw = process.env[envVar];
  return raw && raw.length > 0 ? raw : fallback;
}

export interface InitTelemetryOptions {
  serviceName: string;
  serviceVersion?: string;
  tracesEndpoint?: string;
  metricsEndpoint?: string;
}

export async function initTelemetry(
  opts: InitTelemetryOptions,
): Promise<void> {
  if (initialized) {
    return;
  }

  // Lazy imports so the module imports cheaply when tests just probe
  // constants, and so a missing OTel dep doesn't crash bootstrap.
  let api:
    | typeof import('@opentelemetry/api')
    | undefined;
  try {
    api = await import('@opentelemetry/api');
    const { Resource } = await import('@opentelemetry/resources');
    const {
      ATTR_SERVICE_NAME,
      ATTR_SERVICE_VERSION,
    } = await import('@opentelemetry/semantic-conventions');
    const { NodeTracerProvider } = await import(
      '@opentelemetry/sdk-trace-node'
    );
    const { BatchSpanProcessor } = await import(
      '@opentelemetry/sdk-trace-base'
    );
    const { OTLPTraceExporter } = await import(
      '@opentelemetry/exporter-trace-otlp-http'
    );
    const {
      MeterProvider,
      PeriodicExportingMetricReader,
    } = await import('@opentelemetry/sdk-metrics');
    const { OTLPMetricExporter } = await import(
      '@opentelemetry/exporter-metrics-otlp-http'
    );

    const tracesUrl =
      opts.tracesEndpoint ??
      otlpEndpoint(
        'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT',
        otlpEndpoint(
          'OTEL_EXPORTER_OTLP_ENDPOINT',
          DEFAULT_OTLP_TRACES_ENDPOINT,
        ),
      );
    const metricsUrl =
      opts.metricsEndpoint ??
      otlpEndpoint(
        'OTEL_EXPORTER_OTLP_METRICS_ENDPOINT',
        DEFAULT_OTLP_METRICS_ENDPOINT,
      );

    const resource = new Resource({
      [ATTR_SERVICE_NAME]: opts.serviceName,
      [ATTR_SERVICE_VERSION]: opts.serviceVersion ?? '0.1.0',
      'service.namespace': 'kars',
      'kars.runtime.kind': 'LangGraph',
      'kars.runtime.language': 'typescript',
    });

    const tracerProvider = new NodeTracerProvider({ resource });
    tracerProvider.addSpanProcessor(
      new BatchSpanProcessor(new OTLPTraceExporter({ url: tracesUrl })),
    );
    tracerProvider.register();

    const meterProvider = new MeterProvider({
      resource,
      readers: [
        new PeriodicExportingMetricReader({
          exporter: new OTLPMetricExporter({ url: metricsUrl }),
        }),
      ],
    });
    api.metrics.setGlobalMeterProvider(meterProvider);

    await instrumentHttp();

    initialized = true;
    // eslint-disable-next-line no-console
    console.error(
      `[kars-runtime-langgraph-ts] OTel initialized: traces=${tracesUrl} metrics=${metricsUrl}`,
    );
  } catch (err) {
    // Telemetry must never crash the agent.
    // eslint-disable-next-line no-console
    console.warn(
      '[kars-runtime-langgraph-ts] initTelemetry failed:',
      err,
    );
  }
}

async function instrumentHttp(): Promise<void> {
  try {
    const { registerInstrumentations } = await import(
      '@opentelemetry/instrumentation'
    );
    const { HttpInstrumentation } = await import(
      '@opentelemetry/instrumentation-http'
    );
    // Node.js 22 / undici fetch coverage is best-effort. The
    // dedicated undici instrumentation, when available, captures the
    // built-in fetch.
    const instrumentations: Array<unknown> = [new HttpInstrumentation()];
    try {
      const undici = await import('@opentelemetry/instrumentation-undici');
      // The export name varies across versions; probe defensively.
      const Cls =
        (undici as { UndiciInstrumentation?: new () => unknown })
          .UndiciInstrumentation ??
        undefined;
      if (Cls) {
        instrumentations.push(new Cls());
      }
    } catch {
      // Optional dep missing — fall back to http instrumentation only.
    }
    registerInstrumentations({
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      instrumentations: instrumentations as any,
    });
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(
      '[kars-runtime-langgraph-ts] HTTP instrumentation failed:',
      err,
    );
  }
}

/** Test-only hook to reset the module-level init flag. */
export function _resetForTests(): void {
  initialized = false;
}
