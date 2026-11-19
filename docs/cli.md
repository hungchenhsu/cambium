# Command-line client

`cambium-cli` is a thin client over the [operations API](api/operations.md)
([ADR-0019](adr/0019-cli.md)). It has one command per API call and adds no
behavior of its own.

```sh
cargo build --release -p cambium-cli
export CAMBIUM_TOKEN="$(python3 -c 'import json; print(json.load(open(".cambium-bootstrap.json"))["token"])')"
target/release/cambium-cli operations
target/release/cambium-cli call create_repository --input '{"name":"project"}' --idempotency-key create-project
target/release/cambium-cli --json call list_repositories
```

| Option | Meaning |
| --- | --- |
| `--url` | API base URL. Defaults to `CAMBIUM_URL`, then `http://127.0.0.1:8080`. Plain HTTP only in this version |
| `--token-file` | Read the token from a file. Otherwise `CAMBIUM_TOKEN` is used. Tokens are never accepted as arguments |
| `--json` | Print exactly the API's JSON response body on one line, for success and failure alike |
| `call <operation>` | `POST /v1/operations/<operation>` |
| `--input` | A JSON object, `@path` to read a file, or `-` for standard input. Defaults to `{}` |
| `--idempotency-key` | Sent as `Idempotency-Key`. Reuse it with the same input to retry a mutation safely |
| `operations` | List the server's operations, from MCP `tools/list` |

Exit status: `0` success, `1` API error, `2` usage error, `3` the server could
not be reached or did not answer with an API response. With `--json`, standard
output contains only the response body, so scripts can branch on
`error.code`. Without it, results are indented JSON, and errors, retry hints,
and replay notices go to standard error as messages from the selected Fluent catalog.

The CLI selects the first nonempty value of `LC_ALL`, `LC_MESSAGES`, and
`LANG`. It removes any codeset or modifier suffix, treats underscores as
hyphens, and matches the entire language tag without regard to ASCII case.
For example, `zh_TW.UTF-8` selects [Traditional Chinese (Taiwan)](../crates/cambium-cli/locales/zh-TW/cli.ftl).
An unset or unsupported locale selects [English](../crates/cambium-cli/locales/en/cli.ftl).
With `--json`, diagnostics also use English. API values and operation
descriptions come from the server. Clap supplies command help and syntax errors.
