use std::path::PathBuf;

fn main() {
    let (mut out, mut seed, mut force) = (None, 0u64, false);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--seed" => {
                seed = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| die("--seed attend un entier"))
            }
            "--force" => force = true,
            _ => die(&format!("argument inconnu : {a}")),
        }
    }
    let out =
        out.unwrap_or_else(|| die("usage : moria-worldgen --out <fichier> [--seed N] [--force]"));
    let old = moria_core::world::save_version(&out)
        .unwrap_or_else(|e| die(&format!("lecture impossible : {e}")));
    match old {
        Some(v) if v != moria_core::world::SAVE_VERSION => {
            let mut bak = out.as_os_str().to_owned();
            bak.push(format!(".v{v}.bak"));
            std::fs::rename(&out, &bak)
                .unwrap_or_else(|e| die(&format!("mise de côté impossible : {e}")));
            println!(
                "ancien monde v{v} mis de côté : {}",
                PathBuf::from(bak).display()
            );
        }
        Some(_) if !force => {
            println!("monde déjà présent, rien à faire");
            return;
        }
        _ => {}
    }
    let w = moria_core::world::generate(seed);
    moria_core::world::save(&w, &out)
        .unwrap_or_else(|e| die(&format!("écriture impossible : {e}")));
    println!("{} montagnes générées", w.mountains.len());
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}
