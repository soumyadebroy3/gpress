//! The command-line tool's Flate flags: `--fast N`, the effort, the strong encoder's tuning flags and the
//! thread count, each checked against the library's own output for the same settings.

use gpress::flate::{self, zlib, Options};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Text with numbers and repeats, so that the levels give different outputs.
fn input() -> Vec<u8> {
    let mut s = 7u32;
    let mut out = Vec::new();
    while out.len() < 300_000 {
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.extend_from_slice(
            format!("{} {} m {} {} l S\n", s % 600, s / 7 % 800, s / 11 % 600, s / 13 % 800).as_bytes(),
        );
        if s.is_multiple_of(5) && out.len() > 1000 {
            let back = out.len() / 2;
            let piece = out[back..back + 200].to_vec();
            out.extend_from_slice(&piece);
        }
    }
    out
}

/// `gpress deflate IN OUT ARGS...`, the output read back.
fn deflate_cli(dir: &Path, data: &[u8], args: &[&str]) -> Vec<u8> {
    let (i, o) = (dir.join("in.bin"), dir.join("out.z"));
    std::fs::write(&i, data).unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_gpress")).arg("deflate").arg(&i).arg(&o).args(args).status().unwrap();
    assert!(st.success(), "{args:?}");
    std::fs::read(&o).unwrap()
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("gpress-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_flate_flags_mean_what_the_library_means() {
    let dir = scratch();
    let data = input();
    let lib = |o: Options| flate::deflate_zlib(&data, &o);
    // --fast N: the fast encoder at level N, whatever the effort.
    for level in [1u8, 7, 12] {
        let got = deflate_cli(&dir, &data, &["--fast", &level.to_string(), "--threads", "1"]);
        assert!(got == lib(Options { fast: level, ..Options::effort(100) }), "--fast {level}");
        assert_eq!(zlib::unwrap(&got, data.len() + 1).unwrap().data, data);
    }
    // The effort alone: below 20 the fast encoder, from 20 the strong one.
    for effort in [0u8, 19, 20] {
        let got = deflate_cli(&dir, &data, &["--effort", &effort.to_string(), "--threads", "1"]);
        assert!(got == lib(Options::effort(effort)), "--effort {effort}");
    }
    assert!(Options::effort(0).fast > 0 && Options::effort(20).fast == 0);
    // A tuning flag below 20 selects the strong encoder with that tuning; --fast still wins.
    let tuned = Options { max_chain: 16, fast: 0, ..Options::effort(0) };
    assert!(deflate_cli(&dir, &data, &["--effort", "0", "--chain", "16", "--threads", "1"]) == lib(tuned.clone()));
    assert!(
        deflate_cli(&dir, &data, &["--effort", "0", "--chain", "16", "--fast", "5", "--threads", "1"])
            == lib(Options { fast: 5, ..tuned })
    );
    assert!(deflate_cli(&dir, &data, &["--effort", "0", "--fast", "0", "--threads", "1"]) == lib(Options::quick()));
    // The thread count never changes the output.
    let one = deflate_cli(&dir, &data, &["--effort", "0", "--threads", "1"]);
    for t in ["0", "2", "4"] {
        assert!(deflate_cli(&dir, &data, &["--effort", "0", "--threads", t]) == one, "--threads {t}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
