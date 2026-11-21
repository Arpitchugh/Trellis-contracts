# Synthetic journey monitor

The monitor runs the critical `fund -> oracle quote -> settle` path through the
deterministic sandbox adapters. It checks the expected fee and resulting payer
and payee balances, reports elapsed wall-clock latency, and identifies the
adapter that refused a call when the journey fails.

## Run it

From the repository root:

```sh
cargo run -p testing --bin synthetic-journey-monitor
```

The command prints one JSON object to stdout. Success includes `status`,
`journey`, `latency_ms`, `steps`, `settled_ledger` and `settlement_attempts`.
Failure includes `failed_step`, `dependency`, `detail`, `retryable`,
`settlement_pending` and `settlement_attempts`; it exits with status 1. The
monitor uses a fixed seed and synthetic asset, so the path and settlement values
are reproducible. Wall-clock latency naturally varies between runs.

The journey logic lives in `testing::synthetic_monitor` (Issue #291) and is unit
tested for the normal, failure, retry and authorization paths:

```sh
cargo test -p testing synthetic_monitor
```

### Retry behaviour

A settlement timeout is retried up to `SYNTHETIC_MONITOR_MAX_SETTLEMENT_ATTEMPTS`
times in total (default 3, clamped to 1–10). Retries go through
`Sandbox::retry_pending_settlement`, which resubmits only the settlement leg.
The monitor also asserts that exactly one token transfer happened, so a retry
that paid the recipient twice is reported as a `workflow_assertion` failure.
When the budget is exhausted, the result has `dependency: "rpc"`,
`settlement_pending: true`, `retryable: true`.

### Authorization

The monitor refuses to run (`dependency: "credentials"`) before touching any
adapter when:

- `SYNTHETIC_MONITOR_SECRET` holds any key material (`UnsafeSecret`), or
- `SYNTHETIC_MONITOR_RPC_URL` is not a loopback URL for the local sandbox (`ConfigInvalid`).

A mainnet sandbox configuration is rejected at initialisation (`dependency: "sandbox"`).

### Failure drills

Set `SYNTHETIC_MONITOR_FAULT` to rehearse alerting without editing code:
`oracle_unavailable`, `oracle_stale`, `underfunded_payer`, or `rpc_timeouts:<n>`
(the first `n` settlement submissions time out).

```sh
SYNTHETIC_MONITOR_FAULT=rpc_timeouts:2 cargo run -p testing --bin synthetic-journey-monitor   # recovers, settlement_attempts=3
SYNTHETIC_MONITOR_FAULT=rpc_timeouts:9 cargo run -p testing --bin synthetic-journey-monitor   # fails, retryable
```

`retryable` is `true` only for `oracle` and `rpc` failures. Token, credential,
sandbox and assertion failures need a fix, not a rerun. The output keeps every
field it had before and only adds new ones, so existing parsers keep working.

## Safety and scope

This is a local synthetic check, not a live testnet probe. It uses the existing
`testing::sandbox` implementation: token balances, oracle quotes, and RPC
submissions are all in-process fakes. No network endpoint, wallet secret, or
real funds are used. A passing result verifies the workflow harness and its
expected accounting, but does not prove that deployed contracts are reachable
or correctly wired. Use `scripts/verify-deployment.sh testnet` separately for
read-only checks against a deployment.

## Failures and recovery

| Reported dependency | Example failure | Recovery |
| --- | --- | --- |
| `token` | Payer funding or transfer failed | Check the amount and payer balance in the fixture; restore the expected sandbox funding before retrying. |
| `oracle` | Quote is missing, invalid, or stale | Restore a positive, fresh fixture quote, then rerun the monitor. |
| `rpc` | Synthetic settlement submission failed | Retry the pending settlement path; do not repeat the token transfer, which has already succeeded. |
| `workflow_assertion` | Fee, balances, quote, or credential-free expectation differs | Treat as a behavior regression. Inspect the workflow and fee calculation before changing the expected values. |
| `sandbox` / `workflow` | Setup or an unclassified step failed | Inspect `detail`, correct the harness/configuration issue, and rerun. |

The monitor is read-only with respect to deployed networks. Recovery means
repairing the local fixture or code and rerunning it; it does not attempt to
mutate or roll back an on-chain deployment. For a deployed incident, follow
[`RUNBOOK.md`](RUNBOOK.md), preserve transaction and ledger evidence, and use
the established pause/rollback procedures for the affected contract.
