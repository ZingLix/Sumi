#!/usr/bin/env python3
"""Exercise disposable loopback CI deployments using the public test-only key."""

import argparse
import base64
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.error import URLError
from urllib.parse import urlsplit
from urllib.request import ProxyHandler, Request, build_opener
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-file", type=Path, required=True)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    base = os.environ["SUMI_BASE_URL"].rstrip("/")
    parsed = urlsplit(base)
    if parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
        parser.error("This smoke test only writes synthetic data to loopback HTTP deployments")
    resource = base + "/mcp"
    http = build_opener(ProxyHandler({}))
    deadline = time.monotonic() + 60
    while True:
        try:
            with http.open(base + "/healthz", timeout=2) as response:
                assert response.status == 200
            break
        except (URLError, TimeoutError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.5)
    if args.verify_only:
        state = json.loads(args.state_file.read_text())
        assert state["resource"] == resource
    else:
        if args.state_file.exists():
            parser.error("Use a new state file for a new deployment test")
        state = {"subject": "ci-deployment-" + uuid.uuid4().hex, "resource": resource}
    now = int(time.time())
    header = {"alg": "RS256", "kid": "sumi-test-only", "typ": "at+jwt"}
    claims = {
        "iss": os.environ["SUMI_OAUTH_ISSUER"], "aud": resource,
        "sub": state["subject"], "client_id": "ci-deployment-smoke",
        "scope": "sumi:read sumi:write", "iat": now, "exp": now + 300,
    }
    encode = lambda data: base64.urlsafe_b64encode(data).decode().rstrip("=")
    unsigned = ".".join(encode(json.dumps(v, separators=(",", ":")).encode()) for v in (header, claims))
    key = Path(__file__).resolve().parent.parent / "tests/fixtures/test-key.pem"
    signature = subprocess.check_output(["openssl", "dgst", "-sha256", "-sign", str(key)], input=unsigned.encode())
    token = unsigned + "." + encode(signature)

    def rpc(method, params):
        body = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        request = Request(resource, data=json.dumps(body).encode(), headers={
            "Authorization": "Bearer " + token, "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-11-25",
        })
        with http.open(request, timeout=15) as response:
            result = json.load(response)
        assert "error" not in result, result
        return result["result"]

    def call(name, arguments):
        result = rpc("tools/call", {"name": name, "arguments": arguments})
        assert not result.get("isError"), result
        return result["structuredContent"]

    rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "deployment-smoke", "version": "1"}})
    assert len(rpc("tools/list", {})["tools"]) == 16
    context = call("sumi_context_get", {})
    ledger = context["data"]["ledger_id"]
    if args.verify_only:
        assert ledger == state["ledger_id"]
    else:
        account = call("sumi_accounts_upsert", {"ledger_id": ledger, "idempotency_key": "ci-account", "name": "Synthetic deployment wallet", "kind": "asset", "currency": "CNY"})["data"]["account"]["id"]
        actions = [{"op": "set_opening", "account_id": account, "money": {"currency": "CNY", "amount_minor": "0"}, "as_of": "2025-12-31", "reason": "Synthetic CI baseline"}]
        for kind, amount in [("income", "2000"), ("expense", "1500")]:
            movement = {"kind": kind, "money": {"currency": "CNY", "amount_minor": amount}, "posted_on": "2026-01-02"}
            movement["receipt_account_id" if kind == "income" else "payment_account_id"] = account
            actions.append({"op": "create", "transaction": {"occurred_on": "2026-01-02", "description": "Synthetic deployment " + kind, "movement": movement}})
        request = {"ledger_id": ledger, "idempotency_key": "ci-post", "actions": actions}
        posted = call("sumi_transactions_apply", request)
        assert call("sumi_transactions_apply", request) == posted
        state.update({"ledger_id": ledger, "account_id": account})
        args.state_file.parent.mkdir(parents=True, exist_ok=True)
        args.state_file.write_text(json.dumps(state))
    accounts = call("sumi_accounts_list", {"ledger_id": ledger})["data"]["items"]
    account = next(a for a in accounts if a["account"]["id"] == state["account_id"])
    assert account["balance_minor"] == "500", account
    totals = call("sumi_reports_get", {"ledger_id": ledger, "report": "income_expense"})["data"]["totals"]
    assert len(totals) == 1, totals
    assert totals[0]["income_minor"] == "2000" and totals[0]["gross_expense_minor"] == "1500", totals
    print("Deployment smoke passed: signed MCP, real database writes, idempotency and balances" + (" preserved after restart." if args.verify_only else "."))


if __name__ == "__main__":
    main()
