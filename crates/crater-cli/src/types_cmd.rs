//! `crater types` —— 字段卡。
//!
//! 这条命令存在的理由很具体:在它之前,作者想知道 `systemd_unit` 支持哪些参数,
//! **只能读 Rust 源码**。lint 会在撞墙时告诉你墙在哪(`没有参数 stat,是不是想写 state`),
//! 但那是事后补救,不是让人一开始就看见路。
//!
//! 渲染源是 [`crater_ir::types::BUILTINS`] —— 与 lint 报错、JSON Schema 同一张表,
//! 所以三者**永不互相矛盾**。

use anyhow::{bail, Result};
use crater_ir::types::{self, BuiltinType, Kind, Req};

/// `crater types [<类型>] [--json]`
pub fn run(name: Option<&str>, json: bool) -> Result<()> {
    match name {
        None if json => print_index_json(),
        None => print_index(),
        Some(n) if json => print_card_json(n)?,
        Some(n) => print_card(n)?,
    }
    Ok(())
}

// ---------------------------------------------------------------- 列表

fn print_index() {
    for kind in [Kind::Resource, Kind::Procedural, Kind::Probe, Kind::Declaration] {
        let group: Vec<&BuiltinType> = types::BUILTINS.iter().filter(|t| t.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        println!("{}:", kind.label());
        let width = group.iter().map(|t| t.name.len()).max().unwrap_or(0);
        for t in group {
            // 未实现的类型显式标注 —— 让人 apply 时才撞上是最坏的发现方式。
            let gap = if crater_ir::builtins::get(t.name).is_none() {
                "  [未实现]"
            } else {
                ""
            };
            println!("  {:<width$}  {}{gap}", t.name, t.doc, width = width);
        }
        println!();
    }
    println!("查看某个类型的字段:crater types <类型>");
}

fn print_index_json() {
    // 与 UI 的 /api/types 同一个来源 —— 手拼两遍必然分家。
    println!("{}", crater_ir::types::catalog_json());
}

// ---------------------------------------------------------------- 字段卡

fn print_card(name: &str) -> Result<()> {
    let t = lookup(name)?;

    println!("{} — {}", t.name, t.doc);
    println!("归类: {}", t.kind.label());
    // 声明段条目本就没有五动词实现,不该被说成"尚未实现" —— 那会把
    // "这个类型还欠一份实现"和"这个类型根本不是资源"混为一谈。
    if t.kind != Kind::Declaration && crater_ir::builtins::get(t.name).is_none() {
        println!("状态: **尚未实现**(lint 认得这种写法,但 apply 会失败)");
    }
    println!();

    println!("字段:");
    let width = t.fields.iter().map(|f| f.name.len()).max().unwrap_or(0);
    for f in t.fields {
        println!(
            "  {:<width$}  {:<8}  {:<4}  {}",
            f.name,
            f.ty.label(),
            req_label(f.req),
            f.doc,
            width = width
        );
        if !f.values.is_empty() {
            println!(
                "  {:<width$}  {:<8}  {:<4}  取值: {}",
                "",
                "",
                "",
                f.values.join(" | "),
                width = width
            );
        }
    }

    // 互斥组单独说 —— 光看每行的"择一"看不出是和谁互斥。
    for (group, members) in t.one_of_groups() {
        println!("\n互斥组 `{group}`: {} —— 恰择其一", members.join(" / "));
    }

    if let Some(ff) = t.freeform {
        println!(
            "\n短写法: `- {}: <{}>` 等价于 `- {}: {{ {}: <…> }}`",
            t.name, ff, t.name, ff
        );
    }
    if let Some(note) = t.note {
        println!("\n说明:");
        for line in note.split('\n') {
            println!("  {}", line.trim());
        }
    }

    // 元字段只对资源/探针/过程条目成立;声明段条目没有 target/each/deps。
    if t.kind != Kind::Declaration {
        println!("\n元字段(所有条目通用): name / target / when / each / deps");
    }
    if !t.see_also.is_empty() {
        println!(
            "另见: {}",
            t.see_also
                .iter()
                .map(|s| format!("crater types {s}"))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    Ok(())
}

fn print_card_json(name: &str) -> Result<()> {
    let t = lookup(name)?;
    println!("{}", crater_ir::types::type_json(t.name).expect("lookup 已确认存在"));
    Ok(())
}

/// 查不到时给拼写建议 —— 与 lint 用同一份纠错逻辑。
fn lookup(name: &str) -> Result<&'static BuiltinType> {
    if let Some(t) = types::builtin(name) {
        return Ok(t);
    }
    match types::suggest(name) {
        Some(s) => bail!("没有类型 `{name}`,是不是想写 `{s}`?(`crater types` 看全部)"),
        None => bail!("没有类型 `{name}`。`crater types` 列出全部内建类型"),
    }
}

fn req_label(r: Req) -> &'static str {
    match r {
        Req::Required => "必选",
        Req::Optional => "可选",
        Req::OneOf(_) => "择一",
    }
}




#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_entry_renders_without_panicking() {
        // 字段卡是给人看的第一入口 —— 任何一条渲染不出来都是硬伤。
        for t in types::BUILTINS {
            print_card(t.name).unwrap();
            print_card_json(t.name).unwrap();
        }
    }

    #[test]
    fn an_unknown_type_gets_a_spelling_hint() {
        let err = lookup("servce").unwrap_err().to_string();
        assert!(err.contains("是不是想写 `service`"), "{err}");
        // 完全不像的名字不硬猜,但要告诉人去哪儿看
        let err = lookup("完全不存在的东西").unwrap_err().to_string();
        assert!(err.contains("crater types"), "{err}");
    }

    #[test]
    fn the_card_states_necessity_for_every_field() {
        // "哪些必选哪些可选"正是用户提出的原始诉求。
        for t in types::BUILTINS {
            for f in t.fields {
                assert!(!req_label(f.req).is_empty(), "{}.{} 缺必选性", t.name, f.name);
            }
        }
    }

    #[test]
    fn unimplemented_types_are_flagged_not_hidden() {
        // 登记表如今已全部有实现,所以字段卡上不该出现任何"尚未实现"。
        // 这条断言是防倒退的:再登记新类型而不实现,它立刻变红 ——
        // 让人 apply 时才撞上是最坏的发现方式。
        let pending = crater_ir::builtins::pending();
        assert!(pending.is_empty(), "有登记未实现的类型:{pending:?}");
        // 机制本身仍须完好:真出现 pending 时,卡上必须写明。
        // 声明段条目除外 —— 它没有五动词可实现(见 Kind::Declaration)。
        for t in types::BUILTINS.iter().filter(|t| t.kind != Kind::Declaration) {
            assert!(crater_ir::builtins::get(t.name).is_some(), "{} 没有实现", t.name);
        }
    }
}
