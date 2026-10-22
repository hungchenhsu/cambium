# cambium

> **專案代號，pre-alpha。** Phase 1a 已包含 HTTP／MCP、signed Git 授權、持久化 idempotency，以及會回收無法到達資料的短命分支。可使用[本機 container quickstart](docs/quickstart.md) 試用；第一次從原始碼建置需要額外時間。正式部署所需工作仍列在 Phase 1b。
> 正式產品名稱會在對外推出前決定。

[English](README.md)

Cambium 是開源、以 agent 為主要使用者的 git repo 管理層。agent 透過 API 和 MCP server 就能建立 repo、不 checkout 就寫入變更，並使用用完會自動清理的短命分支。原生 git 照常可用。儲存交給可替換的開源 git 儲存引擎，第一個是 [Enroute](https://github.com/enroute-sh/enroute)。

## 在本機試用

需要 Docker Engine 28+、Compose v2、Python 3.11+ 與 Git 2.50+：

```sh
docker compose up --build -d --wait
```

[Quickstart](docs/quickstart.md) 會建立租戶，並執行 MCP agent：不 checkout 就寫入、讓實驗分支到期、驗證儲存回收，再 clone 保留下來的 commit。只有 API 與 Git 會發布到主機的 loopback port。目前尚未發布 container images；指令會從釘選的原始碼建置。

## 為什麼需要 Cambium

agent 建立 repo、分支和 commit 的速度，超過 git 託管服務當初的設計。現有的開源 server 分成兩類，各自只滿足大量 agent 的部分需求：

- **程式碼託管平台**，例如 Gitea、Forgejo、GitLab：有建立 commit 的 API 和認證機制，Gitea 和 GitLab 也有 MCP server，GitLab 另有 service account。但它們都把每個 repo 以 git 格式存在 server 的磁碟上。
- **物件儲存引擎**，例如 Enroute 和 walgit：擴展性比較好，但認證、租戶和政策都留給架在上面的應用程式處理。

一些早期以 agent 為主的專案補上了部分缺口，例如 agent 帳號或會過期的 repo，但每一個都綁定單一種儲存設計。

Cambium 就是給大量 agent 用的那一層應用程式，架在可以替換的儲存引擎上，提供：

- 租戶隔離
- 區分人和 agent 的身分
- 會回收儲存空間的短命分支
- agent 用的 API 與 MCP server
- 來源資訊
- 合併政策

以衍生資料為基礎、在 server 端完成的歷史查詢排在第二階段。Mononoke 有公開這項能力，但它自稱仍在開發中。

細節與來源見 [docs/comparison.md](docs/comparison.md)（英文，資料截至 2026 年 10 月）。

## 目標

- **以 agent 為主的 API。** 透過 API 和 MCP 建立 repo、寫入變更、讀取歷史，不用 checkout。
- **相容 git。** 原生 `git` 可以執行 `clone`、`fetch`、`push`。後端是 Enroute 時，需要 Git 2.50 以上。
- **能安全承載大量 agent：**
  - 租戶隔離，以及限定範圍的短效 token
  - 區分人和 agent 的身分
  - 有存活期限（TTL）、會自動清理的短命分支
  - 可安全重試（idempotent）的寫入與結構化錯誤
  - 不受語系影響的機器輸出
- **可替換的儲存。** 儲存引擎都接在同一個介面後面，第一個是 Enroute。
- **可自架。** 同一套軟體可以跑成多租戶託管服務，也可以由你自己維運單租戶部署。

## 路線圖

1. **Agent 層。** 階段 1a 是五分鐘內就能試用的最小版本：租戶與 token、API 與 MCP server、不 checkout 的 changeset、短命分支，以及 git 存取，全部架在 Enroute 上。階段 1b 補上正式環境需要的基本功能。
2. **歷史查詢與規模化。** 用衍生資料索引支援 log、檔案歷史、blame 與搜尋，提供事件串流，並接上第二個儲存後端。
3. **Review 與合併政策。** 區分人與 agent 的身分、approval 綁定到特定 commit，並提供風險分級，以及讓外部 review 工具接入的政策 hook。

完整 checklist 見 [ROADMAP.md](ROADMAP.md)（英文），所有設計決策見 [docs/adr](docs/adr/README.md)（英文）。

建置與驗證 workspace 的方式，見[開發指引](docs/development.md)（英文）。

## 先前研究

Cambium 參考了這些專案的設計：

- Meta 的 [Sapling](https://github.com/facebook/sapling) 與其 server [Mononoke](https://github.com/facebook/sapling/blob/main/eden/mononoke/README.md)，特別是 Bonsai changeset 資料模型與衍生資料（derived data）。
- Google 的 Piper。
- Anthropic 的 Scion，見〈[Redesigning source control](https://www.anthropic.com/engineering/redesigning-source-control)〉（2026）。
- [Jujutsu](https://github.com/jj-vcs/jj)，特別是把衝突當成一級公民的設計。

Cambium 不含上述任何專案的程式碼。

## 授權

- Client、協定定義、SDK 與可嵌入的函式庫：Apache-2.0（[LICENSE-APACHE](LICENSE-APACHE)）。
- Server 元件：AGPL-3.0-only（[LICENSE-AGPL](LICENSE-AGPL)）。

repo 裡其餘的內容（包括文件）都是 Apache-2.0。每個 crate 都在 `Cargo.toml` 宣告自己的授權，見 [ADR-0003](docs/adr/0003-licensing.md)。

## 參與貢獻

歡迎貢獻，包括用 AI coding agent 寫的貢獻。每個 commit 都要有 [DCO](https://developercertificate.org/) 簽署（`git commit -s`），細節見 [CONTRIBUTING.md](CONTRIBUTING.md)（英文）。安全問題請依 [SECURITY.md](SECURITY.md)（英文）私下回報。
