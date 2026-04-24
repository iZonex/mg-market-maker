# OTel + Sentry sanity check — runbook

Validates **OBS-1** from TODO: both OTel and Sentry integrations
exist in `crates/server/src/main.rs` + `crates/server/src/otel.rs`,
but neither has been exercised against a live endpoint. This
runbook is what an operator runs once they have real DSNs.

No live exchange keys required. Pure observability sanity — can be
done against any paper-mode run.

---

## Pre-flight

1. **Sentry DSN** — sign up at sentry.io (or point at a self-hosted
   Sentry). Create a project; copy the DSN string of the shape
   `https://<public>@<host>/<project>`.
2. **OTLP endpoint** — stand up a local OTel collector or point
   at a managed one. Minimum: an OTLP-gRPC receiver on port 4317.
   Jaeger all-in-one works:

   ```bash
   docker run -d --rm --name jaeger \
     -p 4317:4317 -p 16686:16686 \
     jaegertracing/all-in-one:latest
   ```

---

## Part A — Sentry (already compiled in, DSN-gated)

```bash
export MM_SENTRY_DSN="https://<public>@sentry.io/<project>"
MM_MODE=paper cargo run --release --bin mm-server
```

On boot stderr should contain:

```
INFO  Sentry enabled (MM_SENTRY_DSN set)
```

Then trigger an error path — fastest is to POST a deliberately
malformed graph:

```bash
# Use an admin JWT — obtain one by logging in through the dashboard
# or via POST /login; there is no dedicated env-var token.
ADMIN_JWT="$(cat admin.jwt)"
curl -X POST http://127.0.0.1:8080/api/admin/strategy/graph \
     -H "Authorization: Bearer $ADMIN_JWT" \
     -H "Content-Type: application/json" \
     -d '{"not":"a graph"}'
```

Expect: Sentry "Issues" tab shows a new event within ~30 s. If
it doesn't arrive:

- Check the server's stderr for `sentry` lines — DSN parse /
  network errors log at `warn!`.
- Confirm egress to `<host>:443` isn't blocked.

### Sentry smoke pass criteria

- [ ] `INFO  Sentry enabled` on boot
- [ ] Event appears in Sentry UI after a deliberate error
- [ ] Breadcrumbs include the preceding `tracing` spans (proves
      `sentry_tracing::layer()` is wired)

---

## Part B — OTel OTLP traces (requires `--features otel` build)

The default build has OTel stubbed to a no-op. For the sanity run
you need the feature on:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
export OTEL_SERVICE_NAME="mm-server"
MM_MODE=paper cargo run --release --features otel --bin mm-server
```

On boot stderr should contain:

```
INFO  OpenTelemetry OTLP enabled (endpoint=http://localhost:4317)
```

Leave it running for 60 s, then open the Jaeger UI at
`http://localhost:16686`. Service dropdown should list `mm-server`.
Select it and hit **Find Traces** — you should see spans per
dashboard HTTP request + per-engine tick.

### OTel smoke pass criteria

- [ ] `INFO  OpenTelemetry OTLP enabled` on boot
- [ ] `mm-server` appears in Jaeger's service list
- [ ] Spans show up with non-empty timing (proves the
      `tracing-opentelemetry` bridge is wired)
- [ ] Clean shutdown flushes pending spans (`Ctrl+C` then check
      the last trace in Jaeger — it should not be truncated)

---

## Teardown

```bash
docker stop jaeger
unset MM_SENTRY_DSN OTEL_EXPORTER_OTLP_ENDPOINT OTEL_SERVICE_NAME
```

If any sanity-pass item failed, file against `TODO.md`'s P1
(observability) band with exact reproduction steps + stderr
excerpt.

---

## Why this isn't automated in CI

- Sentry: needs a real DSN; the ingest endpoint rejects test DSNs.
- OTLP: needs a live collector on the CI runner. Feasible
  (docker-in-docker + jaegertracing/all-in-one) but not worth
  the CI time until either integration has broken once.

Tracked as operator task until it earns CI spend.
