//! Kernelcache kext inventory.
//!
//! The A11 (T8015) iOS 16 kernelcache is the classic *prelinked kernel*
//! format: a FAT-wrapped arm64 Mach-O whose `__PRELINK_INFO,__info` section
//! carries an XML plist with a `PrelinkInfoDictionary` array — one dict per
//! prelinked kext (bundle id, bundle path, load address, and the IOKit
//! personalities XNU uses for driver matching).
//!
//! This module parses that plist with a minimal XML scanner (std-only) and
//! returns the full kext + personality inventory — the authoritative "which
//! driver probes which hardware" table for the QEMU device plan.

use crate::macho;
use crate::Result;

#[derive(Debug, Clone)]
pub struct Personality {
    /// Personality dict key inside IOKitPersonalities.
    pub name: String,
    /// IONameMatch values (string or array).
    pub io_name_match: Vec<String>,
    /// IOProviderClass, if present.
    pub io_provider_class: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KextInfo {
    pub bundle_id: String,
    pub path: String,
    pub load_addr: Option<u64>,
    pub size: Option<u64>,
    pub personalities: Vec<Personality>,
}

// ---------------------------------------------------------------------------
// minimal plist-XML scanning
// ---------------------------------------------------------------------------

fn find_from(hay: &[u8], needle: &str, from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .map(|p| p + from)
}

/// Position just after `<key>{key}</key>` at/after `from`.
fn after_key(xml: &[u8], key: &str, from: usize) -> Option<usize> {
    let pat = format!("<key>{key}</key>");
    find_from(xml, &pat, from).map(|p| p + pat.len())
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// First `<string>...</string>` content at/after `from` (returns value and
/// position after the closing tag).
fn next_string(xml: &[u8], from: usize) -> Option<(String, usize)> {
    let open = find_from(xml, "<string>", from)?;
    let start = open + "<string>".len();
    // tolerate attributes-less <string> only (plist output is uniform)
    let close = find_from(xml, "</string>", start)?;
    let s = std::str::from_utf8(&xml[start..close]).ok()?;
    Some((unescape(s), close + "</string>".len()))
}

/// IONameMatch-style value: either a single `<string>` or an `<array>` of them.
fn string_or_array(xml: &[u8], from: usize) -> Vec<String> {
    let arr = find_from(xml, "<array>", from);
    let s = find_from(xml, "<string>", from);
    match (arr, s) {
        (Some(a), Some(sp)) if a < sp => {
            let end = find_from(xml, "</array>", a).unwrap_or(xml.len());
            let mut out = Vec::new();
            let mut cur = a;
            while let Some((v, nxt)) = next_string(xml, cur) {
                if nxt > end {
                    break;
                }
                out.push(v);
                cur = nxt;
            }
            out
        }
        (Some(a), None) => {
            let end = find_from(xml, "</array>", a).unwrap_or(xml.len());
            let mut out = Vec::new();
            let mut cur = a;
            while let Some((v, nxt)) = next_string(xml, cur) {
                if nxt > end {
                    break;
                }
                out.push(v);
                cur = nxt;
            }
            out
        }
        (_, Some(_)) => next_string(xml, from).map(|(v, _)| vec![v]).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// First `<integer>...</integer>` at/after `from`.
fn next_integer(xml: &[u8], from: usize) -> Option<(u64, usize)> {
    let open = find_from(xml, "<integer>", from)?;
    let start = open + "<integer>".len();
    let close = find_from(xml, "</integer>", start)?;
    let s = std::str::from_utf8(&xml[start..close]).ok()?;
    let v = if let Some(h) = s.trim().strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()?
    } else {
        s.trim().parse::<u64>().ok()?
    };
    Some((v, close))
}

/// Name of the personality = the `<key>` immediately preceding its dict.
fn last_key_before(xml: &[u8], pos: usize) -> String {
    let seg_start = pos.saturating_sub(256);
    let seg = &xml[seg_start..pos];
    match seg.windows(6).rposition(|w| w == b"</key>") {
        Some(ck) => match seg[..ck].windows(5).rposition(|w| w == b"<key>") {
            Some(k) => String::from_utf8_lossy(&seg[k + 5..ck]).trim().to_string(),
            None => "?".into(),
        },
        None => "?".into(),
    }
}

/// Parse the CONTENT of the IOKitPersonalities container (between its
/// `<dict>` and matching `</dict>`) into personalities.
fn parse_personalities(xml: &[u8], start: usize, end: usize) -> Vec<Personality> {
    let mut out = Vec::new();
    for (s, e) in top_level_dicts(xml, start, end) {
        let body = &xml[s..e];
        let name = last_key_before(xml, s);
        let io_name_match =
            after_key(body, "IONameMatch", 0).map(|p| string_or_array(body, p)).unwrap_or_default();
        let io_provider_class = after_key(body, "IOProviderClass", 0)
            .and_then(|p| next_string(body, p).map(|(v, _)| v));
        if !io_name_match.is_empty() || io_provider_class.is_some() {
            out.push(Personality { name, io_name_match, io_provider_class });
        }
    }
    out
}

/// Split `block[from..to]` into top-level `<dict>...</dict>` byte ranges —
/// nesting-aware (the initial `<dict>` is the top-level one; its close is the
/// first `</dict>` seen at nesting depth 0).
fn top_level_dicts(block: &[u8], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = from;
    while let Some(open) = find_from(block, "<dict>", i) {
        if open >= to {
            break;
        }
        let mut depth = 0usize;
        let mut cur = open + 6;
        let mut closed = None;
        while cur < to {
            let next_open = find_from(block, "<dict>", cur);
            let next_close = find_from(block, "</dict>", cur);
            match (next_open, next_close) {
                (Some(o), Some(c)) if o < c => {
                    depth += 1;
                    cur = o + 6;
                }
                (_, Some(c)) => {
                    if depth == 0 {
                        closed = Some((c, c + 7));
                        break;
                    }
                    depth -= 1;
                    cur = c + 7;
                }
                _ => break,
            }
        }
        match closed {
            Some((c, end)) => {
                out.push((open, c));
                i = end;
            }
            None => break,
        }
    }
    out
}

/// Parse the IOKitPersonalities dict block: `<key>...</key><dict> ... </dict>`
/// — find the container dict, then split its content into personality dicts.
fn personalities_of(body: &[u8]) -> Vec<Personality> {
    let Some(p) = after_key(body, "IOKitPersonalities", 0) else { return Vec::new() };
    let Some(co) = find_from(body, "<dict>", p) else { return Vec::new() };
    let ranges = top_level_dicts(body, co, body.len());
    match ranges.first() {
        Some(&(ocs, oce)) => parse_personalities(body, ocs + 6, oce),
        None => Vec::new(),
    }
}

/// Resolve kcgen plist deduplication: `<tag ID="n">value</tag>` definitions
/// and `<tag IDREF="n"/>` references are rewritten into plain XML so the
/// scanners above need no attribute awareness.
fn resolve_idref(xml: &[u8]) -> Vec<u8> {
    // pass 1: collect definitions id -> value
    let mut ids: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    let mut i = 0usize;
    while let Some(p) = find_from(xml, " ID=\"", i) {
        let ns = p + 5;
        let Some(q) = xml[ns..].iter().position(|&c| c == b'"').map(|q| ns + q) else { break };
        let n: u32 = std::str::from_utf8(&xml[ns..q]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let tag_open = xml[..p].iter().rposition(|&c| c == b'<').unwrap_or(0);
        let tag = &xml[tag_open + 1..p];
        let after = q + 1;
        if xml.get(after..after + 2) == Some(b"/>") {
            i = after + 2;
            continue; // reference — resolved in pass 2
        }
        if xml.get(after..after + 1) == Some(b">") {
            let vstart = after + 1;
            let mut close = Vec::new();
            close.extend_from_slice(b"</");
            close.extend_from_slice(tag);
            close.extend_from_slice(b">");
            if let Some(ce) = find_from(xml, &String::from_utf8_lossy(&close), vstart) {
                ids.insert(n, xml[vstart..ce].to_vec());
            }
            i = after;
            continue;
        }
        i = after;
    }
    // pass 2: rewrite tags
    let mut out = Vec::with_capacity(xml.len());
    let mut i = 0usize;
    while i < xml.len() {
        if xml[i] == b'<' {
            let mut j = i + 1;
            while j < xml.len() && xml[j] != b'>' {
                j += 1;
            }
            if j >= xml.len() {
                out.extend_from_slice(&xml[i..]);
                break;
            }
            let tag_s = String::from_utf8_lossy(&xml[i..=j]).into_owned();
            if let Some(idp) = tag_s.find(" IDREF=\"") {
                let rest = &tag_s[idp + 8..];
                if let Some(q) = rest.find('"') {
                    let n: u32 = rest[..q].parse().unwrap_or(0);
                    let raw = &tag_s[1..idp];
                    let name = raw.split([' ', '/']).next().unwrap_or("");
                    out.extend_from_slice(format!("<{name}>").as_bytes());
                    if let Some(v) = ids.get(&n) {
                        out.extend_from_slice(v);
                    }
                    out.extend_from_slice(format!("</{name}>").as_bytes());
                    i = j + 1;
                    continue;
                }
            } else if let Some(idp) = tag_s.find(" ID=\"") {
                let raw = &tag_s[1..idp];
                let name = raw.split([' ', '/']).next().unwrap_or("");
                out.extend_from_slice(format!("<{name}>").as_bytes());
                i = j + 1;
                continue;
            }
            out.extend_from_slice(&xml[i..=j]);
            i = j + 1;
        } else {
            let nxt =
                xml[i..].iter().position(|&c| c == b'<').map(|p| i + p).unwrap_or(xml.len());
            out.extend_from_slice(&xml[i..nxt]);
            i = nxt;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// public entry
// ---------------------------------------------------------------------------

/// Walk the `__PRELINK_INFO,__info` plist of a prelinked kernelcache.
pub fn prelink_kexts(buf: &[u8]) -> Result<Vec<KextInfo>> {
    let (m, slice_off) = macho::parse_best(buf)?;
    let seg = m
        .segments
        .iter()
        .find(|s| s.name == "__PRELINK_INFO")
        .ok_or_else(|| crate::Error::Msg("kernelcache has no __PRELINK_INFO segment".into()))?;
    let sect = seg
        .sections
        .iter()
        .find(|s| s.name == "__info")
        .ok_or_else(|| crate::Error::Msg("__PRELINK_INFO has no __info section".into()))?;
    let start = slice_off + sect.offset as usize;
    let end = (start + sect.size as usize).min(buf.len());
    if start >= end {
        crate::bail!("empty __PRELINK_INFO,__info");
    }
    let xml_owned = resolve_idref(&buf[start..end]);
    let xml = &xml_owned[..];

    let arr = after_key(xml, "_PrelinkInfoDictionary", 0)
        .ok_or_else(|| crate::Error::Msg("no _PrelinkInfoDictionary in prelink info".into()))?;
    // the wrapper opens <array>; find ITS closing </array> with depth counting
    // (kext dicts contain nested arrays that would truncate a naive search)
    let arr_open =
        find_from(xml, "<array>", arr).ok_or_else(|| crate::Error::Msg("no prelink array".into()))?;
    let mut depth = 0usize;
    let mut cur = arr_open + 7; // past the wrapper's own <array>
    let arr_end = loop {
        let o = find_from(xml, "<array>", cur);
        let c = find_from(xml, "</array>", cur);
        match (o, c) {
            (Some(oo), Some(cc)) if oo < cc => {
                depth += 1;
                cur = oo + 7;
            }
            (_, Some(cc)) => {
                if depth == 0 {
                    break cc + 8;
                }
                depth -= 1;
                cur = cc + 8;
            }
            _ => crate::bail!("unterminated _PrelinkInfoDictionary array"),
        }
    };

    let mut out = Vec::new();
    for (s, e) in top_level_dicts(xml, arr, arr_end) {
        let body = &xml[s..e]; // all lookups bounded to THIS dict
        let bundle_id = after_key(body, "CFBundleIdentifier", 0)
            .and_then(|p| next_string(body, p).map(|(v, _)| v))
            .unwrap_or_default();
        let path = after_key(body, "_PrelinkBundlePath", 0)
            .and_then(|p| next_string(body, p).map(|(v, _)| v))
            .unwrap_or_default();
        let load_addr = after_key(body, "_PrelinkExecutableLoadAddr", 0)
            .and_then(|p| next_integer(body, p).map(|(v, _)| v));
        let size = after_key(body, "_PrelinkExecutableSize", 0)
            .and_then(|p| next_integer(body, p).map(|(v, _)| v));
        let personalities = personalities_of(body);
        if bundle_id.is_empty() && path.is_empty() {
            continue;
        }
        out.push(KextInfo { bundle_id, path, load_addr, size, personalities });
    }
    Ok(out)
}
