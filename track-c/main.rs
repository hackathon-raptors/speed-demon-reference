// Speed Demon 2026 Track C (Join), optimized entry in Rust, std only (D = 1).
//
// Hand-rolled RFC-4180 scan of the hostile records.csv (quoted commas, "" escapes,
// embedded LFs, mixed CRLF/LF, mid-record-start BOMs), hash join to regions.csv,
// aggregates per country written bytewise-sorted with _UNMATCHED last. The byte
// counts are taken AFTER unescaping. spec: specs/c-join.md; output must match the
// reference's aggregates.txt byte for byte (sha256 gate).

use std::collections::{HashMap, HashSet};
use std::fs;

const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

struct Group {
    count: i64,
    id_sum: i64,
    amount_sum: i64,
    amount_min: i64,
    amount_max: i64,
    names: HashSet<Vec<u8>>,
    name_bytes: i64,
    note_bytes: i64,
    seen: bool,
}

impl Group {
    fn new() -> Group {
        Group { count: 0, id_sum: 0, amount_sum: 0, amount_min: 0, amount_max: 0,
                names: HashSet::new(), name_bytes: 0, note_bytes: 0, seen: false }
    }
}

fn main() {
    let mut data = "/data".to_string();
    let mut out = "/out".to_string();
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--data" => data = args[i + 1].clone(),
            "--out" => out = args[i + 1].clone(),
            _ => {}
        }
        i += 1;
    }

    // regions.csv: region_id -> country index; codes[idx] = the 3-byte code.
    let rb = fs::read(format!("{data}/regions.csv")).expect("read regions.csv");
    let mut region: HashMap<u32, u32> = HashMap::new();
    let mut codes: Vec<[u8; 3]> = Vec::new();
    let mut intern: HashMap<[u8; 3], u32> = HashMap::new();
    {
        let b = &rb;
        let mut i = 0;
        while i < b.len() && b[i] != b'\n' { i += 1; } // skip header line
        i += 1;
        while i < b.len() {
            if !b[i].is_ascii_digit() { break; }
            let mut id = 0u32;
            while i < b.len() && b[i].is_ascii_digit() { id = id * 10 + (b[i] - b'0') as u32; i += 1; }
            i += 1; // comma
            let code = [b[i], b[i + 1], b[i + 2]];
            i += 3;
            if i < b.len() && b[i] == b'\n' { i += 1; }
            let idx = *intern.entry(code).or_insert_with(|| { codes.push(code); (codes.len() - 1) as u32 });
            region.insert(id, idx);
        }
    }
    let unmatched = codes.len() as u32; // one extra group
    let mut groups: Vec<Group> = (0..codes.len() + 1).map(|_| Group::new()).collect();

    // records.csv
    let rec = fs::read(format!("{data}/records.csv")).expect("read records.csv");
    let b = &rec[..];
    let n = b.len();
    let mut p = 0usize;
    let mut first = true; // the first record is the header
    let mut namebuf: Vec<u8> = Vec::with_capacity(64);
    let mut notelen;

    while p < n {
        if b[p..].starts_with(BOM) { p += 3; } // BOM at a record start (incl. file start)
        if p >= n { break; }

        // id (unquoted digits)
        let mut id = 0i64;
        while p < n && b[p].is_ascii_digit() { id = id * 10 + (b[p] - b'0') as i64; p += 1; }
        // header's id field is the text "id"; skip non-digit bytes of the header's first field
        while p < n && b[p] != b',' { p += 1; }
        p += 1; // comma

        // name (quoted or unquoted) -> interned distinct value + byte length
        namebuf.clear();
        read_text(b, &mut p, &mut namebuf);
        let name_len = namebuf.len() as i64;
        p += 1; // comma

        // note (quoted or unquoted) -> only its unescaped byte length is needed
        notelen = read_text_len(b, &mut p);
        p += 1; // comma

        // region_id (unquoted digits)
        let mut rid = 0u32;
        while p < n && b[p].is_ascii_digit() { rid = rid * 10 + (b[p] - b'0') as u32; p += 1; }
        p += 1; // comma

        // amount (unquoted, until terminator)
        let astart = p;
        while p < n && b[p] != b'\r' && b[p] != b'\n' { p += 1; }
        let cents = parse_amount(&b[astart..p]);
        // consume terminator CRLF or LF
        if p < n && b[p] == b'\r' { p += 2; } else if p < n { p += 1; }

        if first { first = false; continue; } // drop the header record

        let gi = *region.get(&rid).unwrap_or(&unmatched) as usize;
        let g = &mut groups[gi];
        g.count += 1;
        g.id_sum += id;
        g.amount_sum += cents;
        if !g.seen { g.amount_min = cents; g.amount_max = cents; g.seen = true; }
        else { if cents < g.amount_min { g.amount_min = cents; } if cents > g.amount_max { g.amount_max = cents; } }
        g.name_bytes += name_len;
        g.note_bytes += notelen;
        if !g.names.contains(namebuf.as_slice()) { g.names.insert(namebuf.clone()); }
    }

    // output: groups with >=1 record, sorted by key bytewise (_UNMATCHED last).
    let mut order: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].count > 0).collect();
    let key = |i: usize| -> Vec<u8> {
        if i == unmatched as usize { b"_UNMATCHED".to_vec() } else { codes[i].to_vec() }
    };
    order.sort_by(|&a, &c| key(a).cmp(&key(c)));

    let mut outbuf: Vec<u8> = Vec::with_capacity(order.len() * 48);
    for &i in &order {
        let g = &groups[i];
        outbuf.extend_from_slice(&key(i));
        outbuf.push(b' ');
        put_i64(&mut outbuf, g.count);
        outbuf.push(b' ');
        put_i64(&mut outbuf, g.id_sum);
        outbuf.push(b' ');
        put_money(&mut outbuf, g.amount_sum);
        outbuf.push(b' ');
        put_money(&mut outbuf, g.amount_min);
        outbuf.push(b' ');
        put_money(&mut outbuf, g.amount_max);
        outbuf.push(b' ');
        put_i64(&mut outbuf, g.names.len() as i64);
        outbuf.push(b' ');
        put_i64(&mut outbuf, g.name_bytes);
        outbuf.push(b' ');
        put_i64(&mut outbuf, g.note_bytes);
        outbuf.push(b'\n');
    }
    fs::write(format!("{out}/aggregates.txt"), &outbuf).expect("write aggregates.txt");
}

/// Read one CSV field into `dst` (cleared by caller), unescaping a quoted field.
/// Leaves `*p` at the delimiter after the field (',' or a terminator byte).
fn read_text(b: &[u8], p: &mut usize, dst: &mut Vec<u8>) {
    let n = b.len();
    if *p < n && b[*p] == b'"' {
        *p += 1;
        while *p < n {
            if b[*p] == b'"' {
                if *p + 1 < n && b[*p + 1] == b'"' { dst.push(b'"'); *p += 2; }
                else { *p += 1; break; } // closing quote
            } else { dst.push(b[*p]); *p += 1; }
        }
    } else {
        while *p < n && b[*p] != b',' && b[*p] != b'\r' && b[*p] != b'\n' { dst.push(b[*p]); *p += 1; }
    }
}

/// Like read_text but returns only the unescaped byte length (no allocation).
fn read_text_len(b: &[u8], p: &mut usize) -> i64 {
    let n = b.len();
    let mut len = 0i64;
    if *p < n && b[*p] == b'"' {
        *p += 1;
        while *p < n {
            if b[*p] == b'"' {
                if *p + 1 < n && b[*p + 1] == b'"' { len += 1; *p += 2; }
                else { *p += 1; break; }
            } else { len += 1; *p += 1; }
        }
    } else {
        while *p < n && b[*p] != b',' && b[*p] != b'\r' && b[*p] != b'\n' { len += 1; *p += 1; }
    }
    len
}

fn parse_amount(f: &[u8]) -> i64 {
    if f.is_empty() { return 0; }
    let mut i = 0;
    let neg = f[0] == b'-';
    if neg { i = 1; }
    let mut whole = 0i64;
    while i < f.len() && f[i] != b'.' {
        whole = whole.wrapping_mul(10).wrapping_add(f[i].wrapping_sub(b'0') as i64);
        i += 1;
    }
    if i + 2 >= f.len() { return 0; } // no ".XX": not an amount (e.g. the header row)
    let frac = ((f[i + 1] - b'0') as i64) * 10 + (f[i + 2] - b'0') as i64;
    let c = whole * 100 + frac;
    if neg { -c } else { c }
}

fn put_i64(out: &mut Vec<u8>, mut a: i64) {
    if a < 0 { out.push(b'-'); a = -a; }
    let mut u = a as u64;
    if u == 0 { out.push(b'0'); return; }
    let mut tmp = [0u8; 20];
    let mut k = 0;
    while u > 0 { tmp[k] = b'0' + (u % 10) as u8; u /= 10; k += 1; }
    while k > 0 { k -= 1; out.push(tmp[k]); }
}

fn put_money(out: &mut Vec<u8>, c: i64) {
    if c < 0 { out.push(b'-'); }
    let a = c.unsigned_abs();
    let whole = a / 100;
    let frac = a % 100;
    put_i64(out, whole as i64);
    out.push(b'.');
    out.push(b'0' + (frac / 10) as u8);
    out.push(b'0' + (frac % 10) as u8);
}
