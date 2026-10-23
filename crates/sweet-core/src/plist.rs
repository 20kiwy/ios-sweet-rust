//! Apple binary property list (bplist00) reader + writer — enough surface for
//! the iOS rootfs surgery (launchd.plist job edits, preference domains,
//! data_ark). Values: dict / array / string / integer / real / boolean / data.

use crate::error::Error;
use crate::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Plist {
    Dict(BTreeMap<String, Plist>),
    Array(Vec<Plist>),
    Str(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    Data(Vec<u8>),
}

impl Plist {
    pub fn as_dict(&self) -> Option<&BTreeMap<String, Plist>> {
        match self {
            Plist::Dict(d) => Some(d),
            _ => None,
        }
    }
    pub fn as_dict_mut(&mut self) -> Option<&mut BTreeMap<String, Plist>> {
        match self {
            Plist::Dict(d) => Some(d),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Plist::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&Vec<Plist>> {
        match self {
            Plist::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Plist::Int(i) => Some(*i),
            _ => None,
        }
    }
}

fn be(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for x in b {
        v = (v << 8) | *x as u64;
    }
    v
}

pub fn parse(buf: &[u8]) -> Result<Plist> {
    if buf.len() < 40 || &buf[..6] != b"bplist" {
        bail!("not a binary plist");
    }
    let t = buf.len() - 32;
    // trailer: unused[5] sortVersion offsetIntSize objectRefSize
    //          numObjects u64 topObject u64 offsetTableOffset u64
    let off_int = buf[t + 6] as usize;
    let ref_int = buf[t + 7] as usize;
    let nobj = be(&buf[t + 8..t + 16]) as usize;
    let top = be(&buf[t + 16..t + 24]) as usize;
    let table = be(&buf[t + 24..t + 32]) as usize;
    if table + nobj * off_int > buf.len() {
        bail!("bplist offset table out of range");
    }
    let offs: Vec<usize> = (0..nobj)
        .map(|i| be(&buf[table + i * off_int..table + (i + 1) * off_int]) as usize)
        .collect();
    let mut r = PReader { buf, offs, ref_int };
    let v = r.obj(top)?;
    Ok(v)
}

struct PReader<'a> {
    buf: &'a [u8],
    offs: Vec<usize>,
    ref_int: usize,
}

impl<'a> PReader<'a> {
    fn refat(&self, o: usize, i: usize) -> Result<usize> {
        let b = self.buf.get(o + i * self.ref_int..o + (i + 1) * self.ref_int);
        match b {
            Some(b) => Ok(be(b) as usize),
            None => bail!("bplist ref out of range"),
        }
    }

    fn obj(&mut self, idx: usize) -> Result<Plist> {
        let o = *self.offs.get(idx).ok_or_else(|| Error::Msg("bplist obj idx".into()))?;
        let m = *self.buf.get(o).ok_or_else(|| Error::Msg("bplist eof".into()))?;
        let kind = m & 0xF0;
        let info = m & 0x0F;
        let size_of = |info: u8, buf: &[u8], mut o: usize| -> Result<(usize, usize)> {
            // (value bytes length, new offset)
            if info == 0x0F {
                // extended: the byte AFTER the marker is an int-object marker
                // whose low 2 bits are the exponent; size = big-endian
                // (1<<exp) bytes (plistlib _get_size parity)
                let m2 = *buf.get(o + 1).ok_or_else(|| Error::Msg("bplist size eof".into()))?;
                o += 2;
                let exp = (m2 & 0x3) as usize;
                let cnt = 1usize << exp;
                let b = buf
                    .get(o..o + cnt)
                    .ok_or_else(|| Error::Msg("bplist size bytes eof".into()))?;
                Ok((be(b) as usize, o + cnt))
            } else {
                Ok((1usize << info, o + 1))
            }
        };
        match kind {
            0x00 => Ok(match info {
                0x0 => Plist::Bool(false), // null
                0x8 => Plist::Bool(false),
                0x9 => Plist::Bool(true),
                0xF => Plist::Int(0), // fill
                _ => bail!("bplist singleton {info:#x}"),
            }),
            0x10 => {
                let (n, vo) = size_of(info, self.buf, o)?;
                let b = self
                    .buf
                    .get(vo..vo + n)
                    .ok_or_else(|| Error::Msg("bplist int eof".into()))?;
                if n == 16 {
                    // 128-bit — treat as unsigned and clamp (rare)
                    Ok(Plist::Int(be(&b[8..16]) as i64))
                } else {
                    let v = be(b);
                    Ok(Plist::Int(match n {
                        1 => v as u8 as i64,
                        2 => v as u16 as i64,
                        4 => v as u32 as i64,
                        _ => v as i64,
                    }))
                }
            }
            0x20 => {
                let (n, vo) = size_of(info, self.buf, o)?;
                let b = self
                    .buf
                    .get(vo..vo + n)
                    .ok_or_else(|| Error::Msg("bplist real eof".into()))?;
                Ok(Plist::Real(match n {
                    4 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
                    _ => bail!("bplist real {n}"),
                }))
            }
            0x40 => {
                // data: nibble = DIRECT length (0-14), 0xF = extended
                let (n, vo) = if info == 0x0F {
                    size_of(info, self.buf, o)?
                } else {
                    (info as usize, o + 1)
                };
                Ok(Plist::Data(
                    self.buf
                        .get(vo..vo + n)
                        .ok_or_else(|| Error::Msg("bplist data eof".into()))?
                        .to_vec(),
                ))
            }
            0x50 => {
                let (n, vo) = if info == 0x0F {
                    size_of(info, self.buf, o)?
                } else {
                    (info as usize, o + 1)
                };
                let b = self
                    .buf
                    .get(vo..vo + n)
                    .ok_or_else(|| Error::Msg(format!("bplist str eof: obj {idx} off {o} n {n}")))?;
                Ok(Plist::Str(String::from_utf8_lossy(b).into_owned()))
            }
            0x60 => {
                let (n, vo) = if info == 0x0F {
                    size_of(info, self.buf, o)?
                } else {
                    (info as usize, o + 1)
                };
                let b = self
                    .buf
                    .get(vo..vo + n * 2)
                    .ok_or_else(|| Error::Msg(format!("bplist u16 eof: obj {idx} off {o} n {n}")))?;
                let chars: Vec<u16> = (0..n)
                    .map(|i| be(&b[i * 2..i * 2 + 2]) as u16)
                    .collect();
                Ok(Plist::Str(String::from_utf16_lossy(&chars)))
            }
            0x80 => {
                let (n, vo) = size_of(info, self.buf, o)?;
                let b = self
                    .buf
                    .get(vo..vo + n)
                    .ok_or_else(|| Error::Msg("bplist uid eof".into()))?;
                Ok(Plist::Int(be(b) as i64))
            }
            0xA0 => {
                // collections: the nibble IS the count (0-14), 0xF = extended
                let (n, vo) = if info == 0x0F {
                    size_of(info, self.buf, o)?
                } else {
                    (info as usize, o + 1)
                };
                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    out.push(self.obj(self.refat(vo, i)?)?);
                }
                Ok(Plist::Array(out))
            }
            0xD0 => {
                let (n, vo) = if info == 0x0F {
                    size_of(info, self.buf, o)?
                } else {
                    (info as usize, o + 1)
                };
                let mut d = BTreeMap::new();
                for i in 0..n {
                    let k = self.obj(self.refat(vo, i)?)?;
                    let v = self.obj(self.refat(vo + n * self.ref_int, i)?)?;
                    match k {
                        Plist::Str(s) => {
                            d.insert(s, v);
                        }
                        other => bail!("bplist non-string key {other:?}"),
                    }
                }
                Ok(Plist::Dict(d))
            }
            _ => bail!("bplist marker {m:#x}"),
        }
    }
}

// ---------------- writer ----------------
// Two-pass: (1) flatten the tree into a flat node list (containers store
// child indices), (2) serialize in index order. No dedup — correctness over
// size (launchd.plist serializes to a few MB at most).

#[derive(Debug)]
enum WNode {
    Dict(Vec<(usize, usize)>),
    Array(Vec<usize>),
    Str(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    Data(Vec<u8>),
}

fn build_nodes(p: &Plist, nodes: &mut Vec<WNode>) -> usize {
    let idx = nodes.len();
    nodes.push(WNode::Bool(false)); // placeholder
    let n = match p {
        Plist::Dict(d) => {
            let mut pairs = Vec::with_capacity(d.len());
            for (k, v) in d {
                let ki = nodes.len();
                nodes.push(WNode::Str(k.clone()));
                let vi = build_nodes(v, nodes);
                pairs.push((ki, vi));
            }
            WNode::Dict(pairs)
        }
        Plist::Array(a) => {
            let mut kids = Vec::with_capacity(a.len());
            for v in a {
                kids.push(build_nodes(v, nodes));
            }
            WNode::Array(kids)
        }
        Plist::Str(s) => WNode::Str(s.clone()),
        Plist::Int(i) => WNode::Int(*i),
        Plist::Real(f) => WNode::Real(*f),
        Plist::Bool(b) => WNode::Bool(*b),
        Plist::Data(d) => WNode::Data(d.clone()),
    };
    nodes[idx] = n;
    idx
}

pub fn write(p: &Plist) -> Result<Vec<u8>> {
    let mut nodes: Vec<WNode> = Vec::new();
    let top = build_nodes(p, &mut nodes);
    let n = nodes.len();
    let ref_size = if n < 256 {
        1usize
    } else if n < 65536 {
        2
    } else {
        4
    };
    let put_ref = |out: &mut Vec<u8>, i: usize| {
        let b = (i as u64).to_be_bytes();
        out.extend_from_slice(&b[8 - ref_size..]);
    };

    let mut out: Vec<u8> = b"bplist00".to_vec();
    let mut offs: Vec<u32> = Vec::with_capacity(n);
    for node in &nodes {
        offs.push(out.len() as u32);
        match node {
            WNode::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
            WNode::Int(i) => {
                if *i >= 0 && *i <= 0xFF {
                    out.push(0x10);
                    out.push(*i as u8);
                } else if *i >= -0x8000 && *i <= 0x7FFF {
                    out.push(0x11);
                    out.extend_from_slice(&(*i as i16).to_be_bytes());
                } else if *i >= -0x8000_0000 && *i <= 0x7FFF_FFFF {
                    out.push(0x12);
                    out.extend_from_slice(&(*i as i32).to_be_bytes());
                } else {
                    out.push(0x13);
                    out.extend_from_slice(&(*i as i64).to_be_bytes());
                }
            }
            WNode::Real(f) => {
                out.push(0x23);
                out.extend_from_slice(&f.to_be_bytes());
            }
            WNode::Data(d) => {
                put_count(&mut out, 0x40, d.len());
                out.extend_from_slice(d);
            }
            WNode::Str(s) => {
                if s.is_ascii() {
                    put_count(&mut out, 0x50, s.len());
                    out.extend_from_slice(s.as_bytes());
                } else {
                    let u: Vec<u16> = s.encode_utf16().collect();
                    put_count(&mut out, 0x60, u.len());
                    for c in u {
                        out.extend_from_slice(&c.to_be_bytes());
                    }
                }
            }
            WNode::Array(kids) => {
                put_count(&mut out, 0xA0, kids.len());
                for k in kids {
                    put_ref(&mut out, *k);
                }
            }
            WNode::Dict(pairs) => {
                put_count(&mut out, 0xD0, pairs.len());
                for (k, _) in pairs {
                    put_ref(&mut out, *k);
                }
                for (_, v) in pairs {
                    put_ref(&mut out, *v);
                }
            }
        }
    }
    let table_off = out.len() as u32;
    let off_size = if table_off < 0x100 {
        1
    } else if table_off < 0x10000 {
        2
    } else {
        4
    };
    for o in &offs {
        let b = o.to_be_bytes();
        out.extend_from_slice(&b[4 - off_size..4]);
    }
    // trailer: unused[5] sortVersion offsetIntSize objectRefSize
    //          numObjects u64 topObject u64 offsetTableOffset u64
    let mut t = [0u8; 32];
    t[6] = off_size as u8;
    t[7] = ref_size as u8;
    t[8..16].copy_from_slice(&(n as u64).to_be_bytes());
    t[16..24].copy_from_slice(&(top as u64).to_be_bytes());
    t[24..32].copy_from_slice(&(table_off as u64).to_be_bytes());
    out.extend_from_slice(&t);
    Ok(out)
}

/// Collections (arrays/dicts): the nibble IS the count for 0..15.
fn put_count(out: &mut Vec<u8>, marker: u8, len: usize) {
    if len < 15 {
        out.push(marker | len as u8);
    } else {
        out.push(marker | 0x0F);
        // int-object marker: 0x10 | exp, size = 1<<exp big-endian bytes
        let bytes = (len as u64).to_be_bytes();
        let start = bytes.iter().position(|b| *b != 0).unwrap_or(7);
        let nbytes = 8 - start;
        out.push(0x10 | nbytes.ilog2() as u8);
        out.extend_from_slice(&bytes[start..]);
    }
}

