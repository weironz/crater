//! 行号定位 —— 让诊断可点击(`file.yaml:42`)。
//!
//! `serde_yaml` 的 `Value` 不带 span,而"报错必须指到行"是 lint 好不好用的分水岭。
//! 这里用一趟轻量扫描重建"某个 section 的第 N 个列表项在第几行":YAML 已经被 serde
//! 解析通过,所以缩进结构必然良构,扫描只需处理**块标量**(`|` / `>`)这一个陷阱 ——
//! 块标量内部的 `- xxx` 是内容不是列表项。

/// 一份 YAML 源码的结构索引(只保留非空、非注释、非块标量内容的行)。
pub struct LineIndex {
    /// (缩进, 去空白内容, 1-based 行号)
    lines: Vec<(usize, String, usize)>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut lines = Vec::new();
        // 块标量状态:进入后,所有缩进 > 宿主 key 缩进的行都是字面内容。
        let mut block_indent: Option<usize> = None;

        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let indent = raw.len() - raw.trim_start().len();
            let trimmed = raw.trim();

            if let Some(host) = block_indent {
                if trimmed.is_empty() || indent > host {
                    continue; // 仍在块标量里
                }
                block_indent = None;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // `key: |` / `key: >-` / `- |` 等 → 后续更深缩进的行是字面内容
            if is_block_scalar_header(trimmed) {
                block_indent = Some(indent);
            }
            lines.push((indent, trimmed.to_string(), line_no));
        }
        LineIndex { lines }
    }

    /// `path` 逐层下钻(如 `["procedures", "bootstrap", "steps"]`),
    /// 返回该层下每个 `- ` 列表项的行号,顺序与解析顺序一致。
    pub fn list_items(&self, path: &[&str]) -> Vec<usize> {
        let Some((start, end, _)) = self.block_of(path) else {
            return Vec::new();
        };
        // 列表项 = 该块内缩进最浅的那批 `- ` 开头行。
        let Some(item_indent) = self.lines[start..end]
            .iter()
            .filter(|(_, c, _)| c.starts_with("- "))
            .map(|(ind, _, _)| *ind)
            .min()
        else {
            return Vec::new();
        };
        self.lines[start..end]
            .iter()
            .filter(|(ind, c, _)| *ind == item_indent && c.starts_with("- "))
            .map(|(_, _, ln)| *ln)
            .collect()
    }

    /// `path` 指向的 key 自身所在行。
    pub fn key_line(&self, path: &[&str]) -> Option<usize> {
        let (mut lo, mut hi, mut parent_indent) = (0usize, self.lines.len(), None::<usize>);
        let mut found = None;
        for key in path {
            let (i, indent) = self.find_key(lo, hi, key, parent_indent)?;
            found = Some(self.lines[i].2);
            lo = i + 1;
            hi = self.end_of_block(lo, indent);
            parent_indent = Some(indent);
        }
        found
    }

    /// 定位 `path` 指向的 key 之下的内容块 → (起, 止, key 缩进)。
    fn block_of(&self, path: &[&str]) -> Option<(usize, usize, usize)> {
        let (mut lo, mut hi, mut parent_indent) = (0usize, self.lines.len(), None::<usize>);
        let mut indent = 0;
        for key in path {
            let (i, ind) = self.find_key(lo, hi, key, parent_indent)?;
            indent = ind;
            lo = i + 1;
            hi = self.end_of_block(lo, ind);
            parent_indent = Some(ind);
        }
        Some((lo, hi, indent))
    }

    /// 在 [lo, hi) 内找 `key:`;给定父缩进时只认直接子级(最浅的那层)。
    fn find_key(
        &self,
        lo: usize,
        hi: usize,
        key: &str,
        parent_indent: Option<usize>,
    ) -> Option<(usize, usize)> {
        let target_indent = self.lines[lo..hi]
            .iter()
            .filter(|(ind, _, _)| parent_indent.is_none_or(|p| *ind > p))
            .map(|(ind, _, _)| *ind)
            .min()?;
        self.lines[lo..hi]
            .iter()
            .enumerate()
            .find(|(_, (ind, c, _))| {
                *ind == target_indent
                    && (c == &format!("{key}:") || c.starts_with(&format!("{key}: ")))
            })
            .map(|(off, (ind, _, _))| (lo + off, *ind))
    }

    /// 从 `from` 开始,第一条缩进 <= `indent` 的行即块结束。
    fn end_of_block(&self, from: usize, indent: usize) -> usize {
        self.lines[from..]
            .iter()
            .position(|(ind, _, _)| *ind <= indent)
            .map(|off| from + off)
            .unwrap_or(self.lines.len())
    }
}

/// `key: |`、`key: >-`、`key: |+` 等块标量头。
fn is_block_scalar_header(trimmed: &str) -> bool {
    let Some((_, rhs)) = trimmed.rsplit_once(':') else {
        return false;
    };
    let rhs = rhs.trim();
    matches!(rhs.chars().next(), Some('|') | Some('>'))
        && rhs
            .chars()
            .skip(1)
            .all(|c| c == '-' || c == '+' || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"name: demo
params:
  port: { type: port, default: 9000 }
resources:
  - file: { path: /a, state: directory }
  - copy:
      dest: /etc/x
      content: |
        - this is NOT a list item
        - neither is this
  - service: { name: s }
procedures:
  boot:
    steps:
      - shell: "a"
      - shell: "b"
"#;

    #[test]
    fn finds_list_item_lines_at_top_level() {
        let idx = LineIndex::new(SRC);
        assert_eq!(idx.list_items(&["resources"]), vec![5, 6, 11]);
    }

    #[test]
    fn block_scalar_content_is_not_mistaken_for_list_items() {
        // content: | 里那两行 `- …` 必须不算 resources 的项。
        let idx = LineIndex::new(SRC);
        assert_eq!(idx.list_items(&["resources"]).len(), 3);
    }

    #[test]
    fn drills_into_nested_paths() {
        let idx = LineIndex::new(SRC);
        assert_eq!(idx.list_items(&["procedures", "boot", "steps"]), vec![15, 16]);
        assert_eq!(idx.key_line(&["procedures", "boot"]), Some(13));
    }

    #[test]
    fn missing_paths_are_empty_not_panics() {
        let idx = LineIndex::new(SRC);
        assert!(idx.list_items(&["nope"]).is_empty());
        assert!(idx.list_items(&["procedures", "ghost", "steps"]).is_empty());
        assert_eq!(idx.key_line(&["nope"]), None);
    }

    #[test]
    fn key_line_points_at_the_key_itself() {
        let idx = LineIndex::new(SRC);
        assert_eq!(idx.key_line(&["resources"]), Some(4));
        assert_eq!(idx.key_line(&["params", "port"]), Some(3));
    }
}
