use std::process::Command;

fn run(out: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_moria-worldgen"))
        .arg("--out")
        .arg(out)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn existing_world_is_kept_and_exit_is_ok() {
    let dir = std::env::temp_dir().join(format!("moria-wg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("world.bin");
    assert!(run(&f, &[]).status.success());
    let first = std::fs::read(&f).unwrap();
    let again = run(&f, &["--seed", "9"]);
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("monde déjà présent"));
    assert_eq!(std::fs::read(&f).unwrap(), first, "never overwrites");
    assert!(run(&f, &["--seed", "9", "--force"]).status.success());
    assert_ne!(std::fs::read(&f).unwrap(), first, "--force regenerates");
    std::fs::remove_dir_all(dir).unwrap();
}
