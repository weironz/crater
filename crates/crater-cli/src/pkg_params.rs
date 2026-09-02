//! 打包时覆盖参数默认值 —— `crater push … --set version=4.44.3`(issue #25)。
//!
//! **为什么是改写文本,而不是把覆盖值另存一处。**
//!
//! 包解开之后是一个目录,里面那份蓝图就是"这个包是什么"的正身:人会读它、
//! 改它、`git diff` 它,`crater apply -f <那份蓝图>` 也能直接跑。如果覆盖值
//! 存在别处(包的 config、或者一份 overrides 文件),解出来的蓝图就会**写着
//! 一套、装的是另一套** —— 这个仓库反复栽在这类事上(D-159 的假版本号、
//! D-154 的空转标志)。所以覆盖直接烤进文本:包里那份蓝图**自己**说自己是
//! 4.44.3。
//!
//! **为什么逐行改,而不是 serde 读回再写出。**
//!
//! 后者会把注释、空行、键序全抹平,而库里的蓝图有大半价值在注释上
//! (`# stage: build —— 这个参数在烤闭包时就要定死`)。解包的人读到的应该
//! 还是那份蓝图,不是它的骨架。同一条理由见 `pkg::repoint_app`。
//!
//! **改不动就报错,绝不猜。** 认三种写法;都不匹配时明说是哪一个参数。
//! 静默跳过的后果是"`--set` 没报错但也没生效",而那种包会一路发到索引里。

use std::collections::BTreeMap;

/// 把 `sets` 里的键值烤进蓝图文本的 `params:` 段。
///
/// `declared` 是这份蓝图**已声明**的参数名 —— 拿它挡拼写错误:`--set verison=…`
/// 不该静默地什么都不做。
pub(crate) fn bake_defaults(
    text: &str,
    sets: &BTreeMap<String, String>,
    declared: &[String],
) -> Result<String, String> {
    if sets.is_empty() {
        return Ok(text.to_string());
    }
    for k in sets.keys() {
        if !declared.iter().any(|d| d == k) {
            let near = closest(k, declared);
            return Err(format!(
                "`--set {k}=…`:蓝图里没有声明参数 `{k}`{}。\n已声明的:{}",
                near.map(|n| format!("(是不是想写 `{n}`?)"))
                    .unwrap_or_default(),
                if declared.is_empty() {
                    "(一个都没有)".to_string()
                } else {
                    declared.join(", ")
                }
            ));
        }
    }

    let lines: Vec<&str> = text.lines().collect();
    let params_at =
        find_params(&lines).ok_or_else(|| "蓝图里没有 `params:` 段 —— 无处可覆盖".to_string())?;
    let params_indent = indent_of(lines[params_at]);

    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let mut done: BTreeMap<String, bool> = sets.keys().map(|k| (k.clone(), false)).collect();

    let mut i = params_at + 1;
    while i < out.len() {
        let line = out[i].clone();
        let trimmed = line.trim_start();
        // 空行与注释不结束 `params:` 段 —— 库里的蓝图正是靠注释在讲解参数。
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        // 缩进回到 `params:` 这一层或更浅 → 段结束
        if indent_of(&line) <= params_indent {
            break;
        }
        let Some((key, rest)) = split_key(trimmed) else {
            i += 1;
            continue;
        };
        let Some(want) = sets.get(key) else {
            i += 1;
            continue;
        };

        let key_indent = indent_of(&line);
        if rest.trim_start().starts_with('{') {
            // ① 内联 flow map:`version: { default: "4.53.6", stage: build }`
            out[i] = replace_in_flow(&line, want)
                .ok_or_else(|| format!("参数 `{key}` 的内联写法里没有 `default:`:{line}"))?;
        } else if rest.trim().is_empty() {
            // ② 块式:`version:` 后面缩进更深的若干行里找 `default:`
            let mut j = i + 1;
            let mut hit = None;
            while j < out.len() {
                let l = out[j].clone();
                let t = l.trim_start();
                if t.is_empty() || t.starts_with('#') {
                    j += 1;
                    continue;
                }
                if indent_of(&l) <= key_indent {
                    break;
                }
                if let Some((k2, r2)) = split_key(t) {
                    if k2 == "default" {
                        hit = Some((j, l.clone(), r2.to_string()));
                        break;
                    }
                }
                j += 1;
            }
            let (at, l, r) = hit.ok_or_else(|| {
                format!("参数 `{key}` 是块式写法,但里面没有 `default:` —— 无处可覆盖")
            })?;
            out[at] = format!(
                "{}default: {}",
                &l[..l.len() - l.trim_start().len()],
                render(want, r.trim())
            );
        } else {
            // ③ 裸标量:`version: "4.53.6"`
            out[i] = format!(
                "{}{key}: {}",
                &line[..line.len() - line.trim_start().len()],
                render(want, rest.trim())
            );
        }
        done.insert(key.to_string(), true);
        i += 1;
    }

    let missed: Vec<&str> = done
        .iter()
        .filter(|(_, v)| !**v)
        .map(|(k, _)| k.as_str())
        .collect();
    if !missed.is_empty() {
        return Err(format!(
            "`params:` 段里找不到这些参数的定义:{} —— 声明过但没在段内出现,\
             多半是它被 `crater fmt --split` 拆到别的文件去了。\
             把它 `--join` 回来再打包。",
            missed.join(", ")
        ));
    }
    // 原文有没有末尾换行,保持一致 —— 差一个换行会让整包 digest 变。
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// 顶层 `params:`(顶格且独占一行)。
fn find_params(lines: &[&str]) -> Option<usize> {
    lines
        .iter()
        .position(|l| l.trim_end() == "params:" && indent_of(l) == 0)
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// `key: rest` → `(key, rest)`;不是这个形状返回 None。
fn split_key(trimmed: &str) -> Option<(&str, &str)> {
    let i = trimmed.find(':')?;
    let key = trimmed[..i].trim();
    if key.is_empty() || key.contains(' ') || key.starts_with('#') {
        return None;
    }
    Some((key, &trimmed[i + 1..]))
}

/// 在内联 flow map 里换掉 `default:` 的值。
///
/// 只动 `default` 那一段,`stage` / `desc` / `type` 原样留着 —— 覆盖版本号
/// 不该顺手把作者写的说明改了。
fn replace_in_flow(line: &str, want: &str) -> Option<String> {
    let open = line.find('{')?;
    let close = line.rfind('}')?;
    let inner = &line[open + 1..close];
    let mut parts: Vec<String> = Vec::new();
    let mut hit = false;
    for seg in split_top_commas(inner) {
        match split_key(seg.trim()) {
            Some(("default", v)) => {
                hit = true;
                parts.push(format!("default: {}", render(want, v.trim())));
            }
            _ => parts.push(seg.trim().to_string()),
        }
    }
    hit.then(|| {
        format!(
            "{}{{ {} }}{}",
            &line[..open],
            parts.join(", "),
            &line[close + 1..]
        )
    })
}

/// 按逗号切,但**不切引号里的逗号** —— `desc: "先 a, 再 b"` 是一段,不是两段。
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut buf, mut q) = (String::new(), None::<char>);
    for c in s.chars() {
        match (q, c) {
            (Some(qc), c2) if c2 == qc => {
                q = None;
                buf.push(c);
            }
            (Some(_), _) => buf.push(c),
            (None, '"') | (None, '\'') => {
                q = Some(c);
                buf.push(c);
            }
            (None, ',') => {
                out.push(std::mem::take(&mut buf));
            }
            (None, _) => buf.push(c),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

/// 新值怎么写进去 —— **跟着原值的引号风格走**。
///
/// 这一条不是洁癖:`version: "1.2"` 是字符串,写成 `version: 1.2` 就成了浮点数,
/// 拼进 URL 会变成什么取决于序列化器。原来带引号的,覆盖后也带引号;原来没引号
/// 但新值会被误读成别的类型的,自己加上。
fn render(want: &str, original: &str) -> String {
    let quoted = original.starts_with('"') || original.starts_with('\'');
    if quoted || needs_quotes(want) {
        format!("\"{}\"", want.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        want.to_string()
    }
}

/// 不加引号会被 YAML 读成别的类型 —— 那就必须加。
fn needs_quotes(v: &str) -> bool {
    v.is_empty()
        || matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        || v.contains(|c: char| ":#{}[],&*?|>'\"%@`".contains(c))
        || v.trim() != v
}

/// 拼写建议:编辑距离 ≤2 且不超过词长一半。
fn closest<'a>(word: &str, cands: &'a [String]) -> Option<&'a str> {
    cands
        .iter()
        .map(|c| (dist(word, c), c.as_str()))
        .filter(|(d, c)| *d <= 2 && *d * 2 <= c.len().max(word.len()))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn dist(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets(kv: &[(&str, &str)]) -> BTreeMap<String, String> {
        kv.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    fn declared(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 库里全是这种写法。
    #[test]
    fn an_inline_flow_map_gets_only_its_default_replaced() {
        let src = "name: yq\nparams:\n  version: { default: \"4.53.6\", stage: build, desc: \"上游 release tag\" }\n";
        let out = bake_defaults(
            src,
            &sets(&[("version", "4.44.3")]),
            &declared(&["version"]),
        )
        .unwrap();
        assert!(out.contains("default: \"4.44.3\""), "{out}");
        // 其余字段原样 —— 覆盖版本号不该顺手改了作者的说明
        assert!(out.contains("stage: build"), "{out}");
        assert!(out.contains("desc: \"上游 release tag\""), "{out}");
    }

    #[test]
    fn a_block_form_param_works_too() {
        let src = "params:\n  version:\n    default: \"1.0\"\n    stage: build\n";
        let out =
            bake_defaults(src, &sets(&[("version", "2.0")]), &declared(&["version"])).unwrap();
        assert!(out.contains("    default: \"2.0\""), "{out}");
        assert!(out.contains("    stage: build"), "{out}");
    }

    #[test]
    fn a_bare_scalar_param_works_too() {
        let src = "params:\n  version: \"1.0\"\n";
        let out =
            bake_defaults(src, &sets(&[("version", "2.0")]), &declared(&["version"])).unwrap();
        assert!(out.contains("version: \"2.0\""), "{out}");
    }

    /// 注释是这些蓝图一半的价值,不能被抹掉。
    #[test]
    fn comments_and_blank_lines_survive() {
        let src = "params:\n  # 这一行解释了 version 为什么是 build 期\n\n  version: { default: \"1\" }\n\nmaterials: []\n";
        let out = bake_defaults(src, &sets(&[("version", "2")]), &declared(&["version"])).unwrap();
        assert!(
            out.contains("# 这一行解释了 version 为什么是 build 期"),
            "{out}"
        );
        assert!(out.contains("materials: []"), "{out}");
        assert!(out.contains("default: \"2\""), "{out}");
    }

    /// `version: "1.2"` 是字符串。写成 `version: 1.2` 就成了浮点数,
    /// 拼进 URL 会变成什么取决于序列化器 —— 那是最难查的一类。
    #[test]
    fn a_quoted_value_stays_quoted() {
        let src = "params:\n  version: { default: \"1.0\" }\n";
        let out =
            bake_defaults(src, &sets(&[("version", "1.2")]), &declared(&["version"])).unwrap();
        assert!(out.contains("default: \"1.2\""), "{out}");
    }

    /// 原来没引号的数字保持裸的;会被误读成别的类型的自己加引号。
    #[test]
    fn an_ambiguous_value_gets_quoted_even_if_the_original_was_not() {
        let src = "params:\n  port: { default: 8080 }\n";
        let out = bake_defaults(src, &sets(&[("port", "9090")]), &declared(&["port"])).unwrap();
        assert!(out.contains("default: 9090"), "数字该保持裸的:{out}");

        let src = "params:\n  flag: { default: 1 }\n";
        let out = bake_defaults(src, &sets(&[("flag", "true")]), &declared(&["flag"])).unwrap();
        assert!(
            out.contains("default: \"true\""),
            "会被读成 bool,要加引号:{out}"
        );
    }

    /// `desc: "先 a, 再 b"` 里的逗号不是分隔符。
    #[test]
    fn a_comma_inside_quotes_does_not_split_the_flow_map() {
        let src = "params:\n  v: { default: \"1\", desc: \"先 a, 再 b\" }\n";
        let out = bake_defaults(src, &sets(&[("v", "2")]), &declared(&["v"])).unwrap();
        assert!(out.contains("desc: \"先 a, 再 b\""), "{out}");
        assert!(out.contains("default: \"2\""), "{out}");
    }

    /// 拼错的参数名必须报错并给建议 —— 静默不生效的包会一路发到索引里。
    #[test]
    fn a_misspelled_param_is_an_error_with_a_suggestion() {
        let src = "params:\n  version: { default: \"1\" }\n";
        let e =
            bake_defaults(src, &sets(&[("verison", "2")]), &declared(&["version"])).unwrap_err();
        assert!(e.contains("verison"), "{e}");
        assert!(e.contains("是不是想写 `version`"), "{e}");
    }

    /// 声明了但 `params:` 段里找不到(被 `fmt --split` 拆走了)—— 也要报错,
    /// 不能悄悄什么都不做。
    #[test]
    fn a_param_declared_but_absent_from_the_section_is_an_error() {
        let src = "params:\n  other: { default: \"1\" }\n";
        let e = bake_defaults(
            src,
            &sets(&[("version", "2")]),
            &declared(&["version", "other"]),
        )
        .unwrap_err();
        assert!(e.contains("version"), "{e}");
        assert!(e.contains("--join"), "没提示怎么办:{e}");
    }

    #[test]
    fn no_sets_means_the_text_is_returned_untouched() {
        let src = "params:\n  version: { default: \"1\" }\n";
        assert_eq!(
            bake_defaults(src, &BTreeMap::new(), &declared(&["version"])).unwrap(),
            src
        );
    }

    /// 末尾换行要保持 —— 差一个换行,整个包的 digest 就变了。
    #[test]
    fn the_trailing_newline_is_preserved_either_way() {
        let with = "params:\n  v: { default: \"1\" }\n";
        let without = "params:\n  v: { default: \"1\" }";
        assert!(bake_defaults(with, &sets(&[("v", "2")]), &declared(&["v"]))
            .unwrap()
            .ends_with('\n'));
        assert!(
            !bake_defaults(without, &sets(&[("v", "2")]), &declared(&["v"]))
                .unwrap()
                .ends_with('\n')
        );
    }

    /// `params:` 段之外的同名键不该被误改。
    #[test]
    fn a_same_named_key_outside_params_is_left_alone() {
        let src = "params:\n  version: { default: \"1\" }\nvars:\n  version: \"keep-me\"\n";
        let out = bake_defaults(src, &sets(&[("version", "2")]), &declared(&["version"])).unwrap();
        assert!(out.contains("version: \"keep-me\""), "段外的被改了:{out}");
        assert!(out.contains("default: \"2\""), "{out}");
    }
}
