# Human-facing messages for cambium-cli (ADR-0005, ADR-0019).
# Machine output (--json) never uses this catalog.

api-error = Error { $code } (HTTP { $status }): { $message }
api-error-request = Request ID: { $request_id }
api-error-retryable = This error is retryable. Repeat the same command with the same --idempotency-key.
replayed = This is the stored response of an earlier request with the same idempotency key.
missing-token = No token. Set CAMBIUM_TOKEN or pass --token-file.
unreadable-token = Could not read the token file { $path }.
invalid-input = The input is not a JSON object: { $reason }
unreadable-input = Could not read the input { $source }.
unreachable = Could not reach { $url }: { $reason }
unexpected-response = { $url } answered with HTTP { $status } and no Cambium error object.
operations-header = Operations available on { $url }:
