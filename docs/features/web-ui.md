# Web 看板 `crater ui`(Axum + htmx,只读,D-054)

## 这是什么

`crater ui` 起一个**只读 Web 看板**,展示部署状态(读控制端 Turso 库,见 [task-state.md](task-state.md))。后端 **Axum**,前端 **htmx**——服务端渲染 HTML 片段,htmx 每 5s 轮询刷新,JS 近乎为零。

守 crater 本性:
- **纯 Rust**(axum/hyper/tower/tokio,无 C),musl 一键静态(单二进制 +~13MB)。
- **htmx.js vendor 进仓库 + `include_bytes!` 嵌入二进制** → **气隙零网络可用**,不拉 CDN。
- 默认 **`--bind 127.0.0.1`**(最小攻击面)。
- **UI 是视图**:逻辑全在引擎/CLI(D-036),看板不持任何产品逻辑。

## 用法

```bash
crater ui                          # http://127.0.0.1:8080(只读)
crater ui --bind 0.0.0.0 --port 9000   # 对外暴露(注意:暂无鉴权)
```

| 路由 | 内容 |
|---|---|
| `/` | 页面壳(引 htmx,挂载两个轮询片段) |
| `/api/deployments` | 部署表(按 deployment 聚合:Deployment/Task/Version/Hosts/**Status**/Checked/Last applied;DRIFT 标红) |
| `/api/history` | 活动表(When/Action/Deployment/Task/Host/Result) |
| `/htmx.min.js` | 嵌入的 htmx(离线) |

## 验证（本机）

```bash
crater ui --port 8090 &
curl -s localhost:8090/api/deployments   # → <table>… yq … 2 hosts …</table>
curl -s localhost:8090/api/history       # → apply/delete 行,含 deployment 列
curl -s localhost:8090/htmx.min.js | wc -c   # 50917(嵌入提供)
```

## 漂移显示（D-056）

UI 不连主机(被动只读),所以漂移是 **`--verify`(CLI,有凭据)写进 DB → UI 只读显示**:
- `apply` 成功 → status `ok`(apply 含 verify 阶段);`crater task list --verify -i inv` → 把每台 ok/DRIFT 写回 DB。
- 看板 **Status 列**:`DRIFT x/M` 标红、`ok N/M` 绿、`unknown` 灰;另有 **Checked** 列(上次检测时间)。
- handler 每请求重开 DB,保证读到 CLI 进程刚写的状态。

## 边界 / 后续

- 目前**只读**;后续加从 UI 触发 `apply`/`delete`/`--verify`(写操作,调同一引擎)。
- 对外暴露需鉴权(当前默认仅 localhost);不在 UI/库存明文凭据。

## 关联

- ADR：[D-054](../decisions.md)。数据来源:[task-state.md](task-state.md)(D-051/052/053)。
