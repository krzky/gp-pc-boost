// Native Goose v2026.4 benchmark for gp-pc-boost.
// Measures:
// 1. Stack-allocated growable array + stable element reference throughput (no heap allocator).
// 2. Tight scalar array multi-pass CPU/cache scan.

struct Item { key: i32, val: i32 }

fn xs_next(r: u64) -> u64 {
    var x = r;
    x = x ^ (x << 13);
    x = x ^ (x >> 7);
    x = x ^ (x << 17);
    x
}

fn main() {
    let n_items = 1000000;
    var items: Item[>..] = [];
    var marks: Item&[>..] = [];
    var r: u64 = 12345;
    for i in n_items {
        r = xs_next(r);
        let e: Item& = items.push(Item {
            key: (r % 1000000) as i32,
            val: (i % 97) as i32
        });
        if i % 64 == 0 {
            marks.push(e);
        }
    }

    var total: i64 = 0;
    for it in items {
        total += it.key;
    }
    var mt: i64 = 0;
    for m in marks {
        mt += m.val;
    }

    let n_scan = 2000000;
    let passes = 6;
    var data: i32[>..] = [];
    var r2: u64 = 67890;
    for i in n_scan {
        r2 = xs_next(r2);
        data.push((r2 % 1000) as i32);
    }
    var scan_sum: i64 = 0;
    for p in passes {
        for x in data {
            scan_sum += x ^ p;
        }
    }

    let checksum = total ^ (mt << 16) ^ scan_sum;
    print("GOOSE_BENCH items=", items.len, " marks=", marks.len, " checksum=", checksum);
}
