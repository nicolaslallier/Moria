//! Load check: `cargo run --release -p moria-server --example bots -- <addr> [n=10] [secs=120]`.
//! Exits non-zero if any bot's tick p99 >= 150 ms, it saw fewer than secs*5 intervals,
//! or dug 0 tiles. Needs a freshly generated world (fresh volume): on a re-run the
//! designated tiles are already floor, so dug = 0 and the run fails.
use moria_core::protocol::{ClientMsg, PROTOCOL_VERSION, ServerMsg, encode, read_frame};
use moria_core::srd::{Class, Race};
use moria_core::{DesignationKind, Dir, Tile, TilePos};
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Stats {
    intervals_ms: Vec<f64>,
    dug: usize,
}

fn pct(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s.get(((s.len() as f64 * p) as usize).min(s.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0.0)
}

fn bot(i: usize, addr: String, secs: u64, st: Arc<Mutex<Stats>>) -> anyhow::Result<()> {
    let name = format!("bot{:02}", i + 1);
    let mut s = TcpStream::connect(&addr)?;
    s.set_nodelay(true)?;
    // A stalled server surfaces as a read error (counted as a failure).
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.write_all(&encode(&ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        name,
        password: "motdepasse-bot".into(),
        invite: None,
    }))?;
    match read_frame::<ServerMsg>(&mut s)? {
        ServerMsg::Welcome { needs_hero, .. } => {
            if needs_hero {
                s.write_all(&encode(&ClientMsg::CreateHero {
                    race: Race::ALL[i % Race::ALL.len()],
                    class: Class::ALL[i % Class::ALL.len()],
                }))?;
            }
        }
        m => anyhow::bail!("bot{i}: {m:?}"),
    }
    loop {
        if let ServerMsg::Snapshot(_) = read_frame(&mut s)? {
            break;
        }
    }
    // 10x4 = 40 rock tiles next to the open entrance floor.
    let p = |x, y| TilePos { x, y, z: 0 };
    s.write_all(&encode(&ClientMsg::Designate {
        a: p(6, 20),
        b: p(15, 23),
        kind: DesignationKind::Dig,
    }))?;
    // Mover: 5 moves/s, xorshift for direction.
    let mut w = s.try_clone()?;
    std::thread::spawn(move || {
        let mut r = 0x9E3779B97F4A7C15u64 ^ (i as u64 + 1);
        while let Ok(()) = {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let d = [Dir::N, Dir::S, Dir::E, Dir::W][(r % 4) as usize];
            w.write_all(&encode(&ClientMsg::Move(d)))
        } {
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last: Option<Instant> = None;
    while Instant::now() < end {
        if let ServerMsg::Tick(d) = read_frame(&mut s)? {
            let now = Instant::now();
            let mut g = st.lock().unwrap();
            if let Some(l) = last {
                g.intervals_ms.push((now - l).as_secs_f64() * 1000.0);
            }
            g.dug += d.tiles.iter().filter(|(_, t)| *t == Tile::Floor).count();
            last = Some(now);
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let addr = a.get(1).cloned().unwrap_or_else(|| "127.0.0.1:7777".into());
    let arg = |i: usize, d: u64, what: &str| -> u64 {
        a.get(i).map_or(d, |s| {
            s.parse().unwrap_or_else(|_| {
                eprintln!("invalid {what} {s:?}; usage: bots <addr> [n=10] [secs=120]");
                std::process::exit(2)
            })
        })
    };
    let n = arg(2, 10, "bot count") as usize;
    let secs = arg(3, 120, "duration");
    let stats: Vec<_> = (0..n)
        .map(|_| Arc::new(Mutex::new(Stats::default())))
        .collect();
    let hs: Vec<_> = stats
        .iter()
        .enumerate()
        .map(|(i, st)| {
            let (addr, st) = (addr.clone(), st.clone());
            std::thread::spawn(move || bot(i, addr, secs, st))
        })
        .collect();
    let report = |tag: &str| {
        println!("-- {tag}");
        for (i, st) in stats.iter().enumerate() {
            let g = st.lock().unwrap();
            println!(
                "bot{:02} p50={:.1}ms p99={:.1}ms dug={} ticks={}",
                i + 1,
                pct(&g.intervals_ms, 0.5),
                pct(&g.intervals_ms, 0.99),
                g.dug,
                g.intervals_ms.len()
            );
        }
    };
    let t0 = Instant::now();
    while !hs.iter().all(|h| h.is_finished()) {
        std::thread::sleep(Duration::from_secs(1));
        if t0.elapsed().as_secs().is_multiple_of(10) {
            report(&format!("t={}s", t0.elapsed().as_secs()));
        }
    }
    let mut bad = false;
    for (i, h) in hs.into_iter().enumerate() {
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("bot{:02} failed: {e}", i + 1);
                bad = true;
            }
            Err(_) => {
                eprintln!("bot{:02} panicked", i + 1);
                bad = true;
            }
        }
    }
    report("final");
    for st in &stats {
        let g = st.lock().unwrap();
        bad |= pct(&g.intervals_ms, 0.99) >= 150.0
            || g.dug == 0
            || (g.intervals_ms.len() as u64) < secs * 5;
    }
    println!("{}", if bad { "FAIL" } else { "OK" });
    std::process::exit(bad as i32);
}
