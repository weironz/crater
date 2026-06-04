# 模块准入与晋升(module charter)

> 回答一个治理问题:**什么时候配新建一个内置模块?** 防止"一复杂就建模块"
> 滑向 Ansible 几千模块 + collections 版本矩阵的老路。

## 三层泄压阀

```
shell + check          ← 万能逃逸口:任何需求今天就能做(允许丑)
   ↓ 被第二个交付复用了?
role(数据定义)       ← 封装成带参数的可复用单元,进 library/,OCI 分发,不进引擎
   ↓ 跨交付高频 + 幂等/收敛必须引擎帮忙?
内置模块(Rust enum)  ← 最后才晋升到这里
```

**晋升必须由真实需求驱动**(docker_container 是 rustfs 真踩了三次坑才建的),
不做预测式建模块。

## 准入四条(全满足才建)

1. **通用基础设施**,不是某产品的私有逻辑(守 D-017 引擎零产品知识);
2. **≥2-3 个交付真实复用**,或第一个使用者已暴露出明确的复用形态;
3. **幂等/收敛需要引擎帮忙**——数据(shell+check)表达不了或表达出来必然丑
   (如 docker_container 的指纹收敛);
4. **参数能收敛成小的封闭集**(~10 个字段量级),装不下的部分有诚实的逃逸口
   (`args:`/`shell`),不无限吸收上游全部参数面。

## 成本判据:底层工具自己收敛吗?

| 底层语义 | 模块成本 | 例 |
|---|---|---|
| 命令式(自己不收敛) | 厚:引擎补收敛逻辑,~100-150 行 | `docker run` → docker_container 指纹 |
| 声明式(自带收敛) | 薄:拼命令即可,~30-50 行 | `helm upgrade --install`、`kubectl apply` |

所以 helm / kubectl 这类"看起来大"的需求反而便宜——crater 的增值在**物料侧**
(chart/镜像打进 OCI 离线走),模块本体是薄 lowering。

## 明确不做:代码插件机制

dylib(ABI 噩梦)/ WASM(重机械)/ 外挂进程(破坏单二进制零依赖承诺)都不做。
**crater 的"插件"= role + library/ + OCI 分发**:数据插件而非代码插件。第三方
扩展先写 role;真正通用的走 PR 进主仓(monorepo 策展把质量关,等同 ansible-core)。

## 规模预期与代码组织

- 射程内的模块总量预期 **20-30**(OS 原语 + 容器/k8s 交互),不是 Ansible 的几千
  ——云厂商 API 长尾不在 crater 定位(离线/气隙交付)内。
- 每模块 ≈ enum variant + lowering(30-150 行)+ 测试 + `docs/modules/<名>.md`
  (同提交,见 [modules/README](modules/README.md))。
- `engine.rs::action_op` 长胖后按模块拆 `modules/<名>.rs`,纯机械,到时再动。
