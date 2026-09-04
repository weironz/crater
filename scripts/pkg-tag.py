#!/usr/bin/env python3
"""一份蓝图该发成哪个 tag。

规则一句话:**tag = 这份蓝图实际会装的那个版本**。

    params.version.default      有就用它(yq → 4.44.3、zot → 2.1.17)
    否则 blueprint 的 version:   没有上游版本概念的包(mysql → 1)

为什么不是蓝图的 `version:`:那是**蓝图自己的修订号**,库里全是 `1`。
按它发,所有包都会挤在 tag `1` 上,而"yq 的哪个版本"这个问题就没人能回答。
`pkg push` 自己提醒过这件事:「tag `4.44.3` 与蓝图 version `1` 不同 ——
索引与 install 按 tag 走」。

当前工作流按蓝图的默认参数发布,没有传 `--set`,所以 tag 必须由默认值导出。
CLI 已支持用 `crater push --set version=…` 从同一份蓝图发布多个版本;若工作流
将来也传 build 期参数,tag 必须从同一份有效参数导出,绝不能由人手填写。否则会
重现 D-159:索引声称存在 4.40.5,装下去却是 4.44.3。

用法:
    scripts/pkg-tag.py library/yq/yq.blueprint.yaml   # → 4.44.3
"""

import sys


def tag_of(blueprint_path):
    import yaml

    with open(blueprint_path, encoding="utf-8") as f:
        bp = yaml.safe_load(f) or {}

    params = bp.get("params") or {}
    v = params.get("version")
    # 参数可以写成 `version: {default: "4.44.3", …}`,也可以是裸标量。
    # 两种都要认 —— 只认前者会在后者上静默退回蓝图修订号,发出一个 tag `1`。
    if isinstance(v, dict):
        d = v.get("default")
        if d not in (None, ""):
            return str(d)
    elif v not in (None, ""):
        return str(v)

    ver = bp.get("version")
    if ver in (None, ""):
        raise SystemExit(f"{blueprint_path}:既没有 params.version,也没有顶层 version:")
    return str(ver)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    print(tag_of(sys.argv[1]))
