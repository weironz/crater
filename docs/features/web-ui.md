# Web 看板 `crater ui`(Axum + htmx,D-054/058/099)

## 这是什么

`crater ui` 起一个 Web 看板:展示部署状态(读控制端 Turso 库,见 [task-state.md](task-state.md)),
并可触发 **Verify / Heal / Delete**(写操作走后台任务 + 日志面板,D-099)。后端 **Axum**,前端
**htmx**——服务端渲染 HTML 片段,htmx 轮询刷新,JS 近乎为零。

守 crater 本性:
- **纯 Rust**(axum/hyper/tower/tokio,无 C),musl 一键静态。
- **htmx.js vendor 进仓库 + `include_bytes!` 嵌入二进制** → **气隙零网络可用**,不拉 CDN。
- 默认 **`--bind 127.0.0.1`**(最小攻击面);**对外暴露强制 `--token`**(D-099)。
- **UI 是视图**:逻辑全在引擎/CLI(D-036),看板不持任何产品逻辑。

## 用法

```bash
crater ui                                       # http://127.0.0.1:8080(本机)
crater ui --bind 0.0.0.0 --port 9000 --token t1 # 对外暴露:无 --token 直接拒绝启动
# 浏览器首次访问 http://<host>:9000/?token=t1 → 换 cookie;API 走 Authorization: Bearer t1
```

## 写操作(D-058/D-099)

写动作用**当前目录的 `inventory.yaml`**(约定,类似 AWX 的预配机群凭据);缺失时按钮返回提示。

- **Verify now**(全局):重跑 `task list --verify`,漂移写回 DB。
- **Plan**(每行,D-100):`crater plan <source>`——只读变更预演,无需确认;逐主机的
  `N 会变更, M 已就位` 摘要直接流进任务面板。
- **Heal**(每行):对该 deployment re-apply 自愈,confirm 弹窗。
- **Delete**(每行,D-099):跑该 task 的 `teardown:`。**强确认**——`hx-prompt` 要求**输入部署名**,
  服务端校验 `HX-Prompt` 头与部署名相等才执行(GitHub/AWX 式 type-the-name)。

**后台任务 + 日志面板(D-099)**:写操作不再阻塞 HTTP 请求(真实部署要几分钟)——handler spawn
`crater <args>` 子进程立即返回任务面板,面板每 1s 轮询 `/api/job/{id}` 显示日志尾部;完成时服务端
返回 **htmx 286 状态码**停止轮询(成功绿/失败红 + 错误日志),部署表自己的 5s 轮询随后呈现新状态。
任务面板在独立 `#jobs` 区,不会被部署表自刷写掉。

| 路由 | 内容 |
|---|---|
| `/` `/view/*` | 页面壳 + 各视图(仪表盘/主机/主机组/任务) |
| `/api/deployments` | 部署表(Status 漂移列 + Checked;heal/delete 按钮) |
| `/api/verify` `/api/plan/{dep}` `/api/apply/{dep}` `/api/delete/{dep}` | 操作(POST → 任务面板;plan 只读) |
| `/api/job/{id}` | 任务日志片段(运行中 200,结束 286) |
| `/htmx.min.js` | 嵌入的 htmx(离线) |

## 鉴权(D-099)

- `--token <t>`:中间件统一校验——`?token=`(首访,303 + `Set-Cookie`)/ cookie / `Authorization: Bearer`;
  错误或缺失 → 401(带使用提示)。
- **暴露守卫**:`--bind` 非 localhost 且无 `--token` → 启动即报错(UI 能 apply/delete,不允许裸奔)。

## 验证(本机 curl)

- 鉴权矩阵:无 token 401;`/?token=t1` → 303 + cookie;Bearer 200;错 token 401;
  `--bind 0.0.0.0` 无 token → 启动拒绝。
- 任务流:POST `/api/verify` → 面板;`/api/job/1` 运行中 200(running pill);完成 286 + 日志尾
  (实测一台不可达主机的 `ssh connect … No route to host` 直接呈现在面板里)。
- Delete 门:无 `HX-Prompt`/输错名 → `未删除:输入 'x' 与部署名 'y' 不一致`;输对才往下走。

## 漂移显示(D-056)

UI 不连主机(被动只读),漂移是 **`--verify`(CLI,有凭据)写进 DB → UI 只读显示**:
- `apply` 成功 → status `ok`;verify → 每台 ok/DRIFT 写回 DB。
- **Status 列**:`DRIFT x/M` 红、`ok N/M` 绿、`unknown` 灰;**Checked** 列 = 上次检测时间。
- handler 每请求重开 DB,保证读到 CLI 进程刚写的状态。

## 边界 / 后续

- 任务日志在内存(UI 重启丢失,面板收 286 停轮询并提示);不落盘。
- token 是单一静态令牌(无多用户/审计);TLS 自己包(reverse proxy)。
- 不在 UI/库存明文凭据;写操作凭据始终来自控制端本地 inventory.yaml。

## 关联

- ADR:D-054(看板)、D-058(写操作)、**D-099(任务流+Delete+鉴权)**。数据来源:[task-state.md](task-state.md)。
