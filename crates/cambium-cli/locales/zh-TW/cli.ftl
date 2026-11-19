# Human-facing messages for the Traditional Chinese (Taiwan) locale.

api-error = 錯誤 { $code }（HTTP { $status }）：{ $message }
api-error-request = 請求 ID：{ $request_id }
api-error-retryable = 可重試此請求。請使用相同的 --idempotency-key 再次執行指令。
replayed = 這是先前使用相同 idempotency key 的請求所儲存的回應。
missing-token = 缺少 token。請設定 CAMBIUM_TOKEN 或使用 --token-file。
unreadable-token = 無法讀取 token 檔案 { $path }。
invalid-input = 輸入資料必須是 JSON 物件：{ $reason }
unreadable-input = 無法讀取輸入資料 { $source }。
unreachable = 無法連線至 { $url }：{ $reason }
unexpected-response = { $url } 傳回 HTTP { $status }，但回應中沒有 Cambium 錯誤物件。
operations-header = { $url } 提供的操作：
invalid-operation-name = 操作名稱必須使用小寫 snake_case。
expected-object = 輸入資料必須是 JSON 物件。
unreadable-stdin = 無法讀取標準輸入。
