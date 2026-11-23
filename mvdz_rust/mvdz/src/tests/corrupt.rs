use super::common::*;
use pretty_assertions::assert_eq;
use std::panic::{AssertUnwindSafe, catch_unwind};

fn short_duels() -> Option<[Vec<u8>; 2]> {
    let duel = duel()?;
    Some([200_000, 800_000].map(|len| duel[..len.min(duel.len())].to_vec()))
}

fn no_panic(file: &[u8]) {
    for threads in [1, 4] {
        let result = catch_unwind(AssertUnwindSafe(|| {
            crate::decode(file, threads).map(|demo| demo.len())
        }));
        assert!(
            result.is_ok(),
            "decoder panicked on a corrupt input of {} bytes ({threads} threads)",
            file.len()
        );
    }
}

#[test]
fn garbage_never_panics() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for case in 0..3000 {
        let len = rng.below(400);
        let mut file: Vec<u8> = Vec::from_fn(len, |_| rng.next() as u8);
        if case % 2 == 0 && file.len() >= 6 {
            file[..4].copy_from_slice(b"MVDZ");
            file[4] = 1;
        }
        no_panic(&file);
        readers_agree(&file, "garbage");
    }
}

#[test]
fn mutated_streams_never_panic() -> anyhow::Result<()> {
    let Some(demos) = short_duels() else {
        eprintln!("no demo available, skipping");
        return Ok(());
    };
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for source in demos {
        let file = crate::encode(&source, 1)?;
        assert_eq!(crate::decode(&file, 1)?, source);
        for _ in 0..50 {
            let mut mutated = file.clone();
            match rng.below(4) {
                0 => {
                    for _ in 0..=rng.below(8) {
                        let at = rng.below(mutated.len());
                        mutated[at] ^= 1 << rng.below(8);
                    }
                }
                1 => mutated.truncate(rng.below(mutated.len())),
                2 => {
                    let start = rng.below(mutated.len());
                    let end = (start + rng.below(64)).min(mutated.len());
                    for byte in &mut mutated[start..end] {
                        *byte = rng.next() as u8;
                    }
                }
                _ => {
                    let start = rng.below(mutated.len());
                    let end = (start + rng.below(256)).min(mutated.len());
                    let chunk = mutated[start..end].to_vec();
                    let at = rng.below(mutated.len());
                    mutated.splice(at..at, chunk);
                }
            }
            if let Some(decoded) = readers_agree(&mutated, "mutation") {
                assert!(decoded == source, "a corrupt file decoded to other bytes");
            }
        }
    }
    Ok(())
}
