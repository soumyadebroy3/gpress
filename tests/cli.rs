//! The command-line tool's Flate flags: `--fast N`, the effort, the strong encoder's tuning flags and the
//! thread count, each checked against the library's own output for the same settings; and `--store-budget`.

use gpress::flate::{self, zlib, Options};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Text with numbers and repeats, so that the levels give different outputs.
fn input(len: usize) -> Vec<u8> {
    let mut s = 7u32;
    let mut out = Vec::new();
    while out.len() < len {
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
    let data = input(300_000);
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
    // The thread count never changes the output: an input of more than two of the fast encoder's 1 MiB
    // segments, so that threads take part (the deflate tools use a pool from --threads 1 on).
    let big = input(flate::fast::SEGMENT * 5 / 2);
    let one = deflate_cli(&dir, &big, &["--effort", "0", "--threads", "1"]);
    assert!(one == flate::deflate_zlib(&big, &Options::effort(0)));
    for t in ["2", "3", "4"] {
        assert!(deflate_cli(&dir, &big, &["--effort", "0", "--threads", t]) == one, "--threads {t}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A one-page PDF with two content streams of the same text, unfiltered (a classic cross-reference table).
fn small_pdf() -> Vec<u8> {
    let text = input(20_000);
    let stream = |d: &[u8]| {
        let mut b = format!("<< /Length {} >>\nstream\n", d.len()).into_bytes();
        b.extend_from_slice(d);
        b.extend_from_slice(b"\nendstream");
        b
    };
    let objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>".to_vec(),
        stream(&text),
        stream(&text),
    ];
    let mut d = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(d.len());
        d.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        d.extend_from_slice(body);
        d.extend_from_slice(b"\nendobj\n");
    }
    let x = d.len();
    d.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in &offs {
        d.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    d.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    d
}

/// `--store-budget BYTES` is the library's `store_budget`: the engine's summary (`PDFE_DEBUG`) names it, the
/// output is the same at any budget, and a budget past `MAX_STORE_BUDGET` is refused as the library refuses it.
#[test]
fn the_store_budget_flag_is_the_library_option() {
    let dir = std::env::temp_dir().join(format!("gpress-cli-store-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (i, o) = (dir.join("in.pdf"), dir.join("out.pdf"));
    std::fs::write(&i, small_pdf()).unwrap();
    let run = |args: &[&str]| {
        let _ = std::fs::remove_file(&o);
        let r = Command::new(env!("CARGO_BIN_EXE_gpress"))
            .arg("optimize")
            .arg(&i)
            .arg(&o)
            .args(["--effort", "0", "--threads", "1"])
            .args(args)
            .env("PDFE_DEBUG", "1")
            .output()
            .unwrap();
        (r.status.code(), String::from_utf8_lossy(&r.stderr).into_owned(), std::fs::read(&o).ok())
    };
    let (code, log, default) = run(&[]);
    assert_eq!(code, Some(0), "{log}");
    assert!(log.contains(&format!("store: budget {},", gpress::Options::default().store_budget)), "{log}");
    let default = default.unwrap();
    assert!(default.len() < std::fs::metadata(&i).unwrap().len() as usize);
    for budget in ["0", "4096", &gpress::api::MAX_STORE_BUDGET.to_string()] {
        let (code, log, out) = run(&["--store-budget", budget]);
        assert_eq!(code, Some(0), "--store-budget {budget}: {log}");
        assert!(log.contains(&format!("store: budget {budget},")), "--store-budget {budget}: {log}");
        assert!(out.as_deref() == Some(&default[..]), "--store-budget {budget}");
    }
    let over = (gpress::api::MAX_STORE_BUDGET + 1).to_string();
    let (code, log, out) = run(&["--store-budget", &over]);
    let refused = gpress::Options::default().store_budget(gpress::api::MAX_STORE_BUDGET + 1).check().unwrap_err();
    assert_eq!(code, Some(refused.code()), "{log}");
    assert!(log.contains(&refused.to_string()) && out.is_none(), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}
