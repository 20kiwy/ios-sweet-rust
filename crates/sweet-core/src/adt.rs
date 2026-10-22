//! Apple Device Tree (ADT) parse / serialize / dump — port of `tools/adt.py`.
//!
//! Layout: node = `{u32 nprops, u32 nchildren, props..., children...}`;
//! prop = `{char name[32], u32 size (bit31 = placeholder flag), value padded to 4}`.

use crate::{bail, Result};

#[derive(Debug, Clone, Default)]
pub struct Node {
    /// (name, flags, value)
    pub props: Vec<(String, u32, Vec<u8>)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(name: &str) -> Node {
        Node { props: vec![("name".into(), 0, cstr(name))], children: Vec::new() }
    }

    pub fn name(&self) -> &str {
        for (n, _, v) in &self.props {
            if n == "name" {
                return std::str::from_utf8(&v[..v.iter().position(|&c| c == 0).unwrap_or(v.len())])
                    .unwrap_or("?");
            }
        }
        "?"
    }

    pub fn prop(&self, name: &str) -> Option<&[u8]> {
        self.props.iter().find(|(n, _, _)| n == name).map(|(_, _, v)| v.as_slice())
    }

    pub fn set_prop(&mut self, name: &str, value: Vec<u8>) {
        if let Some(slot) = self.props.iter_mut().find(|(n, _, _)| n == name) {
            slot.2 = value;
        } else {
            self.props.push((name.into(), 0, value));
        }
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name() == name)
    }
}

pub fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

pub fn parse(buf: &[u8]) -> Result<(Node, usize)> {
    parse_at(buf, 0)
}

fn parse_at(buf: &[u8], off: usize) -> Result<(Node, usize)> {
    if off + 8 > buf.len() {
        bail!("ADT node header out of bounds at {off:#x}");
    }
    let nprops = u32(buf, off) as usize;
    let nchildren = u32(buf, off + 4) as usize;
    let mut node = Node::default();
    let mut off = off + 8;
    for _ in 0..nprops {
        if off + 36 > buf.len() {
            bail!("ADT prop out of bounds at {off:#x}");
        }
        let name: String = buf[off..off + 32]
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect();
        let size = u32(buf, off + 32);
        let flags = size >> 31;
        let len = (size & 0x7fff_ffff) as usize;
        if off + 36 + len > buf.len() {
            bail!("ADT prop value out of bounds at {off:#x}");
        }
        node.props.push((name, flags, buf[off + 36..off + 36 + len].to_vec()));
        off += 36 + ((len + 3) & !3);
    }
    for _ in 0..nchildren {
        let (child, next) = parse_at(buf, off)?;
        node.children.push(child);
        off = next;
    }
    Ok((node, off))
}

pub fn serialize(node: &Node) -> Vec<u8> {
    let mut out = Vec::new();
    serialize_into(node, &mut out);
    out
}

fn serialize_into(node: &Node, out: &mut Vec<u8>) {
    out.extend_from_slice(&(node.props.len() as u32).to_le_bytes());
    out.extend_from_slice(&(node.children.len() as u32).to_le_bytes());
    for (name, flags, val) in &node.props {
        let mut n = name.as_bytes().to_vec();
        n.resize(32, 0);
        out.extend_from_slice(&n);
        out.extend_from_slice(&((val.len() as u32) | (flags << 31)).to_le_bytes());
        out.extend_from_slice(val);
        out.extend(std::iter::repeat(0).take(val.len().wrapping_neg() & 3));
    }
    for c in &node.children {
        serialize_into(c, out);
    }
}

/// Property value formatter — port of `adt.py fmt()` (used by `dump`).
fn fmt(v: &[u8], hexall: bool) -> String {
    if !v.is_empty()
        && v.last() == Some(&0)
        && v.len() > 1
        && v[..v.len() - 1].iter().all(|&c| c == 0 || (32..127).contains(&c))
    {
        let s: String = v[..v.len() - 1]
            .iter()
            .map(|&c| if c == 0 { '|' } else { c as char })
            .collect();
        return format!("{s:?}");
    }
    if (v.len() == 4 || v.len() == 8) && !hexall {
        let x = if v.len() == 4 {
            u32(v, 0) as u64
        } else {
            u64v(v, 0)
        };
        return format!("{x:#x}");
    }
    if v.len() % 8 == 0 && v.len() <= 64 {
        let items: Vec<String> =
            (0..v.len() / 8).map(|i| format!("{:#x}", u64v(v, i * 8))).collect();
        return format!("<{}>", items.join(" "));
    }
    if v.len() % 4 == 0 && v.len() <= 64 {
        let items: Vec<String> =
            (0..v.len() / 4).map(|i| format!("{:#x}", u32(v, i * 4))).collect();
        return format!("<{}>", items.join(" "));
    }
    let show = v.len().min(48);
    format!("{}{} ({} bytes)", hex(&v[..show]), if v.len() > 48 { "..." } else { "" }, v.len())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|c| format!("{c:02x}")).collect()
}

fn u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn u64v(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// Indented dump — port of `adt.py dump()`.
pub fn dump(node: &Node, depth: usize, hexall: bool) -> String {
    let mut out = String::new();
    let ind = "  ".repeat(depth);
    out.push_str(&format!("{ind}{} {{\n", node.name()));
    for (n, flags, v) in &node.props {
        if n == "name" {
            continue;
        }
        let ph = if *flags != 0 { " [placeholder]" } else { "" };
        out.push_str(&format!("{ind}  {n}{ph} = {}\n", fmt(v, hexall)));
    }
    for c in &node.children {
        out.push_str(&dump(c, depth + 1, hexall));
    }
    out.push_str(&format!("{ind}}}\n"));
    out
}
