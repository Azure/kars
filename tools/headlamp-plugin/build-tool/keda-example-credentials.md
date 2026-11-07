<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Credentials for the KEDA queue examples

The publisher, consumer and scaler examples use your RabbitMQ and Redis services.
No credentials or Kubernetes Secret definitions are distributed with this example.
Provision the two Secrets below before applying `authentication.yaml` and the
workloads. The existing examples and authentication resources use the `default`
namespace; keep all these resources and your service addresses consistent.

Prepare two private environment files outside your source checkout. Restrict their
permissions to your user, do not commit them, and do not include their contents in
logs. Use one `KEY=value` entry per line, without shell quotes or `export`:

- The queue file must contain `RABBITMQ_CONNECTION_STRING` and
  `REDIS_CONNECTION_STRING`: complete connection URIs for your actual services,
  with URI-encoded credentials where required by the service URI syntax.
- The Redis scaler file must contain `redis_username` and `redis_password`: the
  actual, unencoded Redis credentials used by the Redis connection above.

Set `QUEUE_SECRET_ENV_FILE` and `REDIS_SECRET_ENV_FILE` to those file paths. Then
run these real provisioning commands from this directory:

```sh
: "${QUEUE_SECRET_ENV_FILE:?Set the path to your private queue environment file}"
: "${REDIS_SECRET_ENV_FILE:?Set the path to your private Redis environment file}"
kubectl --namespace default create secret generic queue-connection-secrets \
  --from-env-file="$QUEUE_SECRET_ENV_FILE"
kubectl --namespace default create secret generic redis-scaler-secret \
  --from-env-file="$REDIS_SECRET_ENV_FILE"
kubectl --namespace default apply -f authentication.yaml
```

These commands read credentials from the files, not command-line literal values,
and do not print Secret data. They intentionally fail if a Secret already exists;
use your established secret-management process for updates or rotation. Do not
replace existing deployment credentials with example defaults. Keep the resource
names and keys above unchanged: the publisher, consumer, ScaledJob, ScaledObject
and TriggerAuthentication resources already reference them.

Apply the workload manifests appropriate to the example you are running only after
provisioning the credentials and the required broker services. The RabbitMQ and
Redis TriggerAuthentication resources are retained unchanged from upstream.
