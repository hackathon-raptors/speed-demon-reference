// Speed Demon 2026 Track D (Traverse), optimized entry in Rust, std only (D = 1).
//
// CSR graph + a plain binary-heap Dijkstra per query, queries answered in parallel
// across the P-cores with std threads. Distances are u64 (a path sum can exceed 2^32).
// Output is one line per query in input order. spec: specs/d-traverse.md; the output
// must match the reference's distances.txt byte for byte (sha256 gate).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs;
use std::thread;

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

    let graph = fs::read(format!("{data}/graph.txt")).expect("read graph.txt");
    let (n, edges) = parse_graph(&graph);

    // CSR: offsets per node, then packed (head, weight) in node order.
    let mut off = vec![0u32; n + 1];
    for &(u, _, _) in &edges {
        off[u as usize + 1] += 1;
    }
    for k in 0..n {
        off[k + 1] += off[k];
    }
    let m = edges.len();
    let mut head = vec![0u32; m];
    let mut wt = vec![0u32; m];
    let mut cur = off.clone();
    for &(u, v, w) in &edges {
        let p = cur[u as usize] as usize;
        head[p] = v;
        wt[p] = w;
        cur[u as usize] += 1;
    }
    drop(edges);

    let qbytes = fs::read(format!("{data}/queries.txt")).expect("read queries.txt");
    let queries = parse_queries(&qbytes);
    let mut answers = vec![0i64; queries.len()];

    let nthreads = thread::available_parallelism().map(|p| p.get()).unwrap_or(1).min(queries.len().max(1));
    let chunk = queries.len().div_ceil(nthreads.max(1));
    thread::scope(|scope| {
        for (qc, ac) in queries.chunks(chunk).zip(answers.chunks_mut(chunk)) {
            let (off, head, wt) = (&off, &head, &wt);
            scope.spawn(move || {
                let mut dist = vec![0u64; n];
                let mut stamp = vec![0u32; n];
                let mut gen = 0u32;
                let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
                for (slot, &(s, t)) in ac.iter_mut().zip(qc.iter()) {
                    gen += 1;
                    heap.clear();
                    dist[s as usize] = 0;
                    stamp[s as usize] = gen;
                    heap.push(Reverse((0, s)));
                    let mut ans = -1i64;
                    while let Some(Reverse((d, u))) = heap.pop() {
                        if stamp[u as usize] == gen && d > dist[u as usize] {
                            continue; // stale heap entry
                        }
                        if u == t {
                            ans = d as i64;
                            break;
                        }
                        let (lo, hi) = (off[u as usize] as usize, off[u as usize + 1] as usize);
                        for e in lo..hi {
                            let v = head[e] as usize;
                            let nd = d + wt[e] as u64;
                            if stamp[v] != gen || nd < dist[v] {
                                dist[v] = nd;
                                stamp[v] = gen;
                                heap.push(Reverse((nd, v as u32)));
                            }
                        }
                    }
                    *slot = ans;
                }
            });
        }
    });

    let mut buf = Vec::with_capacity(answers.len() * 4);
    let mut tmp = itoa_buf();
    for &a in &answers {
        write_i64(&mut buf, a, &mut tmp);
        buf.push(b'\n');
    }
    fs::write(format!("{out}/distances.txt"), &buf).expect("write distances.txt");
}

fn parse_graph(b: &[u8]) -> (usize, Vec<(u32, u32, u32)>) {
    let mut i = 0usize;
    let n = next_u64(b, &mut i) as usize;
    let m = next_u64(b, &mut i) as usize;
    let mut edges = Vec::with_capacity(m);
    for _ in 0..m {
        let u = next_u64(b, &mut i) as u32;
        let v = next_u64(b, &mut i) as u32;
        let w = next_u64(b, &mut i) as u32;
        edges.push((u, v, w));
    }
    (n, edges)
}

fn parse_queries(b: &[u8]) -> Vec<(u32, u32)> {
    let mut i = 0usize;
    let mut q = Vec::new();
    while i < b.len() {
        while i < b.len() && !b[i].is_ascii_digit() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let s = next_u64(b, &mut i) as u32;
        let t = next_u64(b, &mut i) as u32;
        q.push((s, t));
    }
    q
}

#[inline]
fn next_u64(b: &[u8], i: &mut usize) -> u64 {
    while *i < b.len() && !b[*i].is_ascii_digit() {
        *i += 1;
    }
    let mut v = 0u64;
    while *i < b.len() && b[*i].is_ascii_digit() {
        v = v * 10 + (b[*i] - b'0') as u64;
        *i += 1;
    }
    v
}

fn itoa_buf() -> [u8; 24] {
    [0u8; 24]
}

#[inline]
fn write_i64(out: &mut Vec<u8>, mut a: i64, tmp: &mut [u8; 24]) {
    if a < 0 {
        out.push(b'-');
        a = -a;
    }
    let mut u = a as u64;
    if u == 0 {
        out.push(b'0');
        return;
    }
    let mut n = 0;
    while u > 0 {
        tmp[n] = b'0' + (u % 10) as u8;
        u /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        out.push(tmp[n]);
    }
}
