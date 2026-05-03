use anyhow::Result;
use hex_literal::hex;
use sha2::{Digest, Sha256};
use std::env::var;
use std::fs::{canonicalize, copy as fs_copy, File};
use std::io::{copy, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use zip::ZipArchive;

const FFTW_WINDOWS_ZIP_URL: &str = "https://fftw.org/pub/fftw/fftw-3.3.5-dll64.zip";
const FFTW_WINDOWS_ZIP_SHA256: [u8; 32] =
    hex!("cfd88dc0e8d7001115ea79e069a2c695d52c8947f5b4f3b7ac54a192756f439f");

fn cargo_target_os() -> String {
    var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS must be set by cargo")
}

fn cargo_target_env() -> Option<String> {
    var("CARGO_CFG_TARGET_ENV").ok()
}

fn command_exists(command: &str) -> bool {
    Command::new(command).arg("--version").output().is_ok()
}

fn resolve_dlltool(target: &str) -> String {
    if let Ok(dlltool) = var("DLLTOOL") {
        if command_exists(&dlltool) {
            return dlltool;
        }
        panic!(
            "Could not find dlltool `{}`. Set DLLTOOL or install mingw-w64 binutils.",
            dlltool
        );
    }

    let mut candidates = vec![];
    if target.starts_with("x86_64-") && target.contains("windows-gnu") {
        candidates.push("x86_64-w64-mingw32-dlltool");
    }
    if target.starts_with("i686-") && target.contains("windows-gnu") {
        candidates.push("i686-w64-mingw32-dlltool");
    }
    candidates.push("dlltool");

    for candidate in candidates {
        if command_exists(candidate) {
            return candidate.to_string();
        }
    }

    panic!("Could not find dlltool. Set DLLTOOL or install mingw-w64 binutils.")
}

fn make_import_lib_msvc(out_dir: &Path, target: &str, stem: &str) {
    run(cc::windows_registry::find_tool(target, "lib.exe")
        .unwrap()
        .to_command()
        .arg("/MACHINE:X64")
        .arg(format!("/DEF:lib{}.def", stem))
        .arg(format!("/OUT:lib{}.lib", stem))
        .current_dir(out_dir))
}

fn make_import_lib_gnu(out_dir: &Path, target: &str, stem: &str) {
    let dlltool = resolve_dlltool(target);
    let output = format!("lib{}.dll.a", stem);
    run(Command::new(dlltool)
        .arg("--input-def")
        .arg(format!("lib{}.def", stem))
        .arg("--dllname")
        .arg(format!("lib{}.dll", stem))
        .arg("--output-lib")
        .arg(&output)
        .current_dir(out_dir));

    let output_path = out_dir.join(output);
    let cargo_link_name_output_path = out_dir.join(format!("liblib{}.dll.a", stem));
    if !cargo_link_name_output_path.exists() {
        fs_copy(output_path, cargo_link_name_output_path).unwrap();
    }
}

fn download_archive_windows(out_dir: &Path) -> Result<()> {
    if out_dir.join("libfftw3-3.def").exists()
        && out_dir.join("libfftw3f-3.def").exists()
        && out_dir.join("libfftw3-3.dll").exists()
        && out_dir.join("libfftw3f-3.dll").exists()
    {
        return Ok(());
    }

    let archive = out_dir.join("fftw_windows.zip");
    if !archive.exists() {
        let buf = ureq::get(FFTW_WINDOWS_ZIP_URL)
            .call()?
            .body_mut()
            .read_to_vec()?;
        let digest = Sha256::digest(&buf);
        if digest != FFTW_WINDOWS_ZIP_SHA256 {
            anyhow::bail!("SHA-256 mismatch for {}", FFTW_WINDOWS_ZIP_URL);
        }

        let mut f = File::create(&archive)?;
        f.write_all(&buf)?;
    }
    let f = File::open(&archive)?;
    let mut zip = ZipArchive::new(f)?;
    for name in &["fftw3-3", "fftw3f-3"] {
        for ext in &["dll", "def"] {
            let filename = format!("lib{}.{}", name, ext);
            let mut zf = zip.by_name(&filename)?;
            let mut f = File::create(out_dir.join(filename))?;
            copy(&mut zf, &mut f)?;
        }
    }
    Ok(())
}

fn build_unix(out_dir: &Path) {
    let src_dir = PathBuf::from(var("CARGO_MANIFEST_DIR").unwrap()).join("fftw-3.3.8");
    let out_src_dir = out_dir.join("src");
    fs_extra::dir::copy(
        src_dir,
        &out_src_dir,
        &fs_extra::dir::CopyOptions {
            overwrite: true,
            skip_exist: false,
            buffer_size: 64000,
            copy_inside: true,
            depth: 0,
            content_only: false,
        },
    )
    .unwrap();
    if !out_dir.join("lib/libfftw3.a").exists() {
        build_fftw(&[], &out_src_dir, out_dir);
    }
    if !out_dir.join("lib/libfftw3f.a").exists() {
        build_fftw(&["--enable-single"], &out_src_dir, out_dir);
    }
}

fn build_fftw(flags: &[&str], src_dir: &Path, out_dir: &Path) {
    run(
        Command::new(canonicalize(src_dir.join("configure")).unwrap())
            .arg("--with-pic")
            .arg("--enable-static")
            .arg("--disable-doc")
            .arg(format!("--prefix={}", out_dir.display()))
            .args(flags)
            .current_dir(src_dir),
    );
    run(Command::new("make")
        .arg(format!("-j{}", var("NUM_JOBS").unwrap()))
        .current_dir(src_dir));
    run(Command::new("make").arg("install").current_dir(src_dir));
}

fn run(command: &mut Command) {
    println!("Running: {:?}", command);
    match command.status() {
        Ok(status) => {
            if !status.success() {
                panic!("`{:?}` failed: {}", command, status);
            }
        }
        Err(error) => {
            panic!("failed to execute `{:?}`: {}", command, error);
        }
    }
}

fn main() {
    let out_dir = PathBuf::from(var("OUT_DIR").unwrap());
    if cargo_target_os() == "windows" {
        let target = var("TARGET").unwrap();
        download_archive_windows(&out_dir).unwrap();

        match cargo_target_env().as_deref() {
            Some("msvc") => {
                for stem in &["fftw3-3", "fftw3f-3"] {
                    make_import_lib_msvc(&out_dir, &target, stem);
                }
            }
            Some("gnu") => {
                for stem in &["fftw3-3", "fftw3f-3"] {
                    make_import_lib_gnu(&out_dir, &target, stem);
                }
            }
            Some(env) => {
                panic!(
                    "Unsupported windows target env `{}` for target `{}`",
                    env, target
                );
            }
            None => {
                panic!("CARGO_CFG_TARGET_ENV must be set for target `{}`", target);
            }
        }

        println!("cargo:rustc-link-search={}", out_dir.display());
        println!("cargo:rustc-link-lib=libfftw3-3");
        println!("cargo:rustc-link-lib=libfftw3f-3");
    } else {
        build_unix(&out_dir);
        println!("cargo:rustc-link-search={}", out_dir.join("lib").display());
        println!("cargo:rustc-link-lib=static=fftw3");
        println!("cargo:rustc-link-lib=static=fftw3f");
    }
}
