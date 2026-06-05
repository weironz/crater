//! Minimal ZIP reader (D-103) — just enough to pull ONE member out of a
//! release zip on the CONTROL side. Upstreams (e.g. RustFS) ship binaries
//! zip-only; targets can't be assumed to have `unzip`, and GNU tar can't read
//! zip — so a `kind: file` material may declare `unzip: <member>` and the
//! control machine extracts the member bytes BEFORE they travel (build pack /
//! online PushFile / offline blob). Pure Rust on flate2 (already a dep):
//! stored (method 0) + deflate (method 8), zip64-aware central directory.

use crate::Result;
use anyhow::{anyhow, bail};
use std::io::Read;

const EOCD_SIG: u32 = 0x0605_4b50; // end of central directory
const EOCD64_LOC_SIG: u32 = 0x0706_4b50; // zip64 EOCD locator
const EOCD64_SIG: u32 = 0x0606_4b50; // zip64 EOCD record
const CDH_SIG: u32 = 0x0201_4b50; // central directory header
const LFH_SIG: u32 = 0x0403_4b50; // local file header

fn u16le(b: &[u8], off: usize) -> u64 {
    u16::from_le_bytes([b[off], b[off + 1]]) as u64
}
fn u32le(b: &[u8], off: usize) -> u64 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as u64
}
fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// One central-directory entry we care about.
struct Entry {
    name: String,
    method: u64,
    comp_size: u64,
    uncomp_size: u64,
    lfh_offset: u64,
}

/// Locate the central directory and parse its entries (zip64-aware).
fn entries(data: &[u8]) -> Result<Vec<Entry>> {
    // EOCD lives in the last 22..=22+65535 bytes (variable comment) — scan back.
    let scan_from = data.len().saturating_sub(22 + 65_536);
    let eocd = (scan_from..data.len().saturating_sub(21))
        .rev()
        .find(|&i| u32le(data, i) as u32 == EOCD_SIG)
        .ok_or_else(|| anyhow!("not a zip: end-of-central-directory not found"))?;
    let mut count = u16le(data, eocd + 10);
    let mut cd_off = u32le(data, eocd + 16);
    // zip64: sentinel values redirect through the EOCD64 locator just before EOCD.
    if (count == 0xFFFF || cd_off == 0xFFFF_FFFF) && eocd >= 20 {
        let loc = eocd - 20;
        if u32le(data, loc) as u32 == EOCD64_LOC_SIG {
            let e64 = u64le(data, loc + 8) as usize;
            if e64 + 56 > data.len() || u32le(data, e64) as u32 != EOCD64_SIG {
                bail!("corrupt zip64 end-of-central-directory");
            }
            count = u64le(data, e64 + 32);
            cd_off = u64le(data, e64 + 48);
        }
    }
    let mut out = Vec::new();
    let mut p = cd_off as usize;
    for _ in 0..count {
        if p + 46 > data.len() || u32le(data, p) as u32 != CDH_SIG {
            bail!("corrupt zip central directory");
        }
        let method = u16le(data, p + 10);
        let mut comp = u32le(data, p + 20);
        let mut uncomp = u32le(data, p + 24);
        let name_len = u16le(data, p + 28) as usize;
        let extra_len = u16le(data, p + 30) as usize;
        let comment_len = u16le(data, p + 32) as usize;
        let mut lfh = u32le(data, p + 42);
        let name = String::from_utf8_lossy(&data[p + 46..p + 46 + name_len]).into_owned();
        // zip64 extra field (id 0x0001): only the fields that hit the u32
        // sentinel are present, in fixed order uncomp, comp, lfh-offset.
        let mut x = p + 46 + name_len;
        let extra_end = x + extra_len;
        while x + 4 <= extra_end {
            let (id, sz) = (u16le(data, x), u16le(data, x + 2) as usize);
            if id == 0x0001 {
                let mut f = x + 4;
                for slot in [&mut uncomp, &mut comp, &mut lfh] {
                    if *slot == 0xFFFF_FFFF && f + 8 <= x + 4 + sz {
                        *slot = u64le(data, f);
                        f += 8;
                    }
                }
            }
            x += 4 + sz;
        }
        out.push(Entry { name, method, comp_size: comp, uncomp_size: uncomp, lfh_offset: lfh });
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// List member names (for error messages / inspection).
pub fn list_members(data: &[u8]) -> Result<Vec<String>> {
    Ok(entries(data)?.into_iter().map(|e| e.name).collect())
}

/// Extract one member's bytes. `member` matches the stored name exactly,
/// modulo a leading `./`.
pub fn extract_member(data: &[u8], member: &str) -> Result<Vec<u8>> {
    let want = member.trim_start_matches("./");
    let all = entries(data)?;
    let e = all
        .iter()
        .find(|e| e.name.trim_start_matches("./") == want)
        .ok_or_else(|| {
            let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).take(20).collect();
            anyhow!("zip 内无成员 '{member}'(包内: {})", names.join(", "))
        })?;
    // Sizes in the central directory are authoritative (set even when the
    // local header deferred them to a data descriptor).
    let lfh = e.lfh_offset as usize;
    if lfh + 30 > data.len() || u32le(data, lfh) as u32 != LFH_SIG {
        bail!("corrupt zip local header for '{}'", e.name);
    }
    let nl = u16le(data, lfh + 26) as usize;
    let el = u16le(data, lfh + 28) as usize;
    let start = lfh + 30 + nl + el;
    let end = start + e.comp_size as usize;
    if end > data.len() {
        bail!("zip 截断:成员 '{}' 数据越界", e.name);
    }
    let raw = &data[start..end];
    match e.method {
        0 => Ok(raw.to_vec()), // stored
        8 => {
            // deflate
            let mut out = Vec::with_capacity(e.uncomp_size as usize);
            flate2::read::DeflateDecoder::new(raw).read_to_end(&mut out)?;
            Ok(out)
        }
        m => bail!("zip 成员 '{}' 用不支持的压缩方法 {m}(只支持 stored/deflate)", e.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Hand-roll a zip with the given (name, bytes, deflate?) members —
    /// the writer side we deliberately don't ship.
    fn make_zip(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        let mut count = 0u16;
        for (name, content, deflate) in members {
            let (method, data) = if *deflate {
                let mut enc =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                enc.write_all(content).unwrap();
                (8u16, enc.finish().unwrap())
            } else {
                (0u16, content.to_vec())
            };
            let crc = crc_stub(content);
            let lfh_off = out.len() as u32;
            out.extend_from_slice(&LFH_SIG.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0]); // version, flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc (we read by CD sizes)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(content.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&data);

            cd.extend_from_slice(&CDH_SIG.to_le_bytes());
            cd.extend_from_slice(&[20, 0, 20, 0, 0, 0]); // made-by, needed, flags
            cd.extend_from_slice(&method.to_le_bytes());
            cd.extend_from_slice(&[0; 4]); // time, date
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(content.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0; 12]); // extra+comment len, disk, int/ext attrs (2+2+2+2+4)
            cd.extend_from_slice(&lfh_off.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
            count += 1;
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        let cd_size = (out.len() as u32) - cd_off;
        out.extend_from_slice(&EOCD_SIG.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // disk numbers
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    fn crc_stub(_b: &[u8]) -> u32 {
        0 // reader doesn't verify crc; size-checked via CD
    }

    #[test]
    fn extracts_stored_and_deflate_members() {
        let z = make_zip(&[
            ("rustfs", b"#!ELF fake binary bytes", true),
            ("README.md", b"docs", false),
        ]);
        assert_eq!(extract_member(&z, "rustfs").unwrap(), b"#!ELF fake binary bytes");
        assert_eq!(extract_member(&z, "README.md").unwrap(), b"docs");
        assert_eq!(extract_member(&z, "./rustfs").unwrap(), b"#!ELF fake binary bytes");
        assert_eq!(list_members(&z).unwrap(), vec!["rustfs", "README.md"]);
    }

    #[test]
    fn missing_member_lists_contents() {
        let z = make_zip(&[("bin/tool", b"x", false)]);
        let err = extract_member(&z, "tool").unwrap_err().to_string();
        assert!(err.contains("bin/tool"), "error should list members, got: {err}");
    }

    #[test]
    fn non_zip_rejected() {
        assert!(extract_member(b"definitely not a zip file at all......", "x").is_err());
    }
}
